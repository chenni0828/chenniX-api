use chennix_common::ProxyResult;
use serde_json::{json, Value};

/// Convert a Claude `/v1/messages` request body to OpenAI `/v1/chat/completions` format.
///
/// Key transformations (reverse of o2c.rs):
/// - `system` array (or string) → `messages[0].role=system` (text blocks concatenated)
/// - `content[].type=text` → `content` string (concatenated)
/// - `content[].type=tool_use` → `tool_calls: [{id, type:"function", function:{name, arguments}}]`
///   (arguments is the JSON string of `input`)
/// - Messages containing `content[].type=tool_result` → emitted as separate
///   `role=tool` messages (one per tool_result block), with `tool_call_id` and `content`
/// - `tools[].input_schema` → `tools[].function.parameters`
/// - `disable_parallel_tool_use` → `parallel_tool_calls` (boolean inverted)
/// - `thinking.budget_tokens` → `reasoning_effort` (<1700→"low", <3000→"medium", else→"high")
/// - `max_tokens`, `temperature`, `top_p`, `model`, `stream` — pass through
pub fn claude_to_openai_request(body: &Value) -> ProxyResult<Value> {
    let mut out = serde_json::Map::new();

    // model — pass through (caller will replace with upstream_model_name)
    if let Some(model) = body.get("model") {
        out.insert("model".to_string(), model.clone());
    }

    let mut messages: Vec<Value> = Vec::new();

    // 1. system (string or array of text blocks) → messages[0].role=system
    if let Some(system) = body.get("system") {
        let system_text = match system {
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                let mut texts: Vec<String> = Vec::new();
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            texts.push(t.to_string());
                        }
                    }
                }
                texts.join("")
            }
            _ => String::new(),
        };
        if !system_text.is_empty() {
            messages.push(json!({"role": "system", "content": system_text}));
        }
    }

    // 2. Walk messages, converting each
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string();

            // Content can be a string or an array of blocks
            let blocks: Vec<Value> = match msg.get("content") {
                Some(Value::Array(arr)) => arr.clone(),
                Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
                _ => Vec::new(),
            };

            // Detect tool_result-containing message: split into multiple tool messages
            let has_tool_result = blocks
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));

            if has_tool_result {
                // Each tool_result → separate role=tool message.
                // Any text blocks → separate user/assistant message (rare but possible).
                let mut text_parts: Vec<String> = Vec::new();
                for block in &blocks {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("tool_result") => {
                            let tool_use_id =
                                block.get("tool_use_id").cloned().unwrap_or(json!(""));
                            let content_val =
                                block.get("content").cloned().unwrap_or(json!(""));
                            // OpenAI tool message content is a string
                            let content_str = match content_val {
                                Value::String(s) => s,
                                Value::Array(arr) => {
                                    let mut parts: Vec<String> = Vec::new();
                                    for b in &arr {
                                        if b.get("type").and_then(|t| t.as_str())
                                            == Some("text")
                                        {
                                            if let Some(t) =
                                                b.get("text").and_then(|t| t.as_str())
                                            {
                                                parts.push(t.to_string());
                                            }
                                        }
                                    }
                                    parts.join("")
                                }
                                other => other.to_string(),
                            };
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content_str
                            }));
                        }
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                if !text_parts.is_empty() {
                    messages.push(json!({"role": role, "content": text_parts.join("")}));
                }
                continue;
            }

            // Regular message: collect text blocks → content string, tool_use → tool_calls
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();

            for block in &blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(t.to_string());
                        }
                    }
                    Some("tool_use") => {
                        let id = block.get("id").cloned().unwrap_or(json!(""));
                        let name = block.get("name").cloned().unwrap_or(json!(""));
                        let input = block.get("input").cloned().unwrap_or(json!({}));
                        let arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments
                            }
                        }));
                    }
                    _ => {}
                }
            }

            let content_str = text_parts.join("");
            let mut msg_obj = serde_json::Map::new();
            msg_obj.insert("role".to_string(), json!(role));
            if !tool_calls.is_empty() {
                // Assistant with tool_calls: content=null when no text (matches OpenAI convention)
                msg_obj.insert(
                    "content".to_string(),
                    if content_str.is_empty() {
                        json!(null)
                    } else {
                        json!(content_str)
                    },
                );
                msg_obj.insert("tool_calls".to_string(), Value::Array(tool_calls));
            } else {
                msg_obj.insert("content".to_string(), json!(content_str));
            }
            messages.push(Value::Object(msg_obj));
        }
    }

    out.insert("messages".to_string(), Value::Array(messages));

    // 3. max_tokens — pass through (OpenAI treats it as optional)
    if let Some(mt) = body.get("max_tokens") {
        out.insert("max_tokens".to_string(), mt.clone());
    }

    // 4. temperature, top_p — pass through
    if let Some(t) = body.get("temperature") {
        out.insert("temperature".to_string(), t.clone());
    }
    if let Some(t) = body.get("top_p") {
        out.insert("top_p".to_string(), t.clone());
    }

    // 5. stream — pass through
    if let Some(s) = body.get("stream") {
        out.insert("stream".to_string(), s.clone());
    }

    // 6. tools conversion: tools[].{name, description, input_schema}
    //    → tools[].{type:"function", function:{name, description, parameters}}
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let openai_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").cloned().unwrap_or(json!(""));
                let input_schema = tool.get("input_schema").cloned().unwrap_or(json!({}));
                let mut func = serde_json::Map::new();
                func.insert("name".to_string(), name);
                if let Some(desc) = tool.get("description") {
                    func.insert("description".to_string(), desc.clone());
                }
                func.insert("parameters".to_string(), input_schema);
                Some(json!({
                    "type": "function",
                    "function": Value::Object(func)
                }))
            })
            .collect();
        if !openai_tools.is_empty() {
            out.insert("tools".to_string(), Value::Array(openai_tools));
        }
    }

    // 7. disable_parallel_tool_use → parallel_tool_calls (inverted)
    if let Some(dptu) = body.get("disable_parallel_tool_use").and_then(|p| p.as_bool()) {
        out.insert("parallel_tool_calls".to_string(), json!(!dptu));
    }

    // 8. thinking.budget_tokens → reasoning_effort (approximate)
    // 阈值与 o2c.rs 正向映射对齐：1280→low, 2048→medium, 4096→high
    if let Some(budget) = body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(|b| b.as_u64())
    {
        let effort = if budget < 1700 {
            "low"
        } else if budget < 3000 {
            "medium"
        } else {
            "high"
        };
        out.insert("reasoning_effort".to_string(), json!(effort));
    }

    Ok(Value::Object(out))
}

/// Convert an OpenAI `/v1/chat/completions` response back to Claude `/v1/messages` format.
///
/// Key transformations:
/// - `choices[0].message.reasoning_content` → `content: [{type:"thinking", thinking:"..."}]`
///   (兼容 reasoning 字段；放在 text block 之前)
/// - `choices[0].message.content` (string) → `content: [{type:"text", text:"..."}]`
/// - `choices[0].message.tool_calls` → `content: [..., {type:"tool_use", id, name, input}]`
///   (`arguments` JSON string is parsed into `input` object)
/// - `choices[0].finish_reason` → `stop_reason`:
///     stop→end_turn, tool_calls→tool_use, length→max_tokens, content_filter→end_turn
/// - `usage.prompt_tokens` → `usage.input_tokens`
/// - `usage.completion_tokens` → `usage.output_tokens`
/// - Wraps as `{id, type:"message", role:"assistant", model, content, stop_reason,
///     stop_sequence:null, usage:{input_tokens, output_tokens}}`
pub fn openai_to_claude_response(body: &Value) -> ProxyResult<Value> {
    let id = body.get("id").cloned().unwrap_or(json!(""));
    let model = body.get("model").cloned().unwrap_or(json!(""));

    let mut content: Vec<Value> = Vec::new();
    let mut stop_reason = "end_turn";

    if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
        if let Some(choice) = choices.first() {
            if let Some(message) = choice.get("message") {
                // Reasoning content → thinking block（兼容 reasoning_content 和 reasoning 字段）
                // 放在 text block 之前，与 Claude 响应顺序一致（先思考后回答）
                let reasoning = message
                    .get("reasoning_content")
                    .and_then(|r| r.as_str())
                    .or_else(|| message.get("reasoning").and_then(|r| r.as_str()));
                if let Some(r) = reasoning {
                    if !r.is_empty() {
                        content.push(json!({"type": "thinking", "thinking": r}));
                    }
                }
                // Text content → text block
                if let Some(text) = message.get("content").and_then(|c| c.as_str()) {
                    if !text.is_empty() {
                        content.push(json!({"type": "text", "text": text}));
                    }
                }
                // Tool calls → tool_use blocks
                if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tool_calls {
                        let id = tc.get("id").cloned().unwrap_or(json!(""));
                        let empty_obj = json!({});
                        let function = tc.get("function").unwrap_or(&empty_obj);
                        let name = function.get("name").cloned().unwrap_or(json!(""));
                        let arguments_str = function
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}");
                        let input: Value =
                            serde_json::from_str(arguments_str).unwrap_or(json!({}));
                        content.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                }
            }
            // finish_reason → stop_reason
            if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                stop_reason = match fr {
                    "stop" => "end_turn",
                    "tool_calls" => "tool_use",
                    "length" => "max_tokens",
                    "content_filter" => "end_turn",
                    _ => "end_turn",
                };
            }
        }
    }

    // Claude requires a non-empty content array
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    // usage mapping
    let empty_obj = json!({});
    let usage_obj = body.get("usage").unwrap_or(&empty_obj);
    let input_tokens = usage_obj
        .get("prompt_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = usage_obj
        .get("completion_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    let mut out = serde_json::Map::new();
    out.insert("id".to_string(), id);
    out.insert("type".to_string(), json!("message"));
    out.insert("role".to_string(), json!("assistant"));
    out.insert("model".to_string(), model);
    out.insert("content".to_string(), Value::Array(content));
    out.insert("stop_reason".to_string(), json!(stop_reason));
    out.insert("stop_sequence".to_string(), json!(null));
    out.insert(
        "usage".to_string(),
        json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        }),
    );

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- claude_to_openai_request tests ----

    #[test]
    fn test_simple_claude_request_to_openai() {
        let input = json!({
            "model": "claude-sonnet-4",
            "system": [{"type": "text", "text": "You are helpful"}],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hello"}]}
            ],
            "max_tokens": 100
        });
        let r = claude_to_openai_request(&input).unwrap();
        assert_eq!(r["model"], "claude-sonnet-4");
        assert_eq!(r["max_tokens"], 100);
        let messages = r["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
    }

    #[test]
    fn test_tool_use_to_tool_calls() {
        let input = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "What's the weather?"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "toolu_abc", "name": "get_weather", "input": {"location": "SF"}}
                ]}
            ],
            "max_tokens": 100
        });
        let r = claude_to_openai_request(&input).unwrap();
        let messages = r["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "Let me check.");
        let tool_calls = messages[1]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "toolu_abc");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        assert!(tool_calls[0]["function"]["arguments"].is_string());
        let args: Value =
            serde_json::from_str(tool_calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({"location": "SF"}));
    }

    #[test]
    fn test_tool_result_to_tool_message() {
        let input = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "What's the weather?"}]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_abc", "name": "get_weather", "input": {"location": "SF"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_abc", "content": "Sunny, 72F"}
                ]}
            ],
            "max_tokens": 100
        });
        let r = claude_to_openai_request(&input).unwrap();
        let messages = r["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "toolu_abc");
        assert_eq!(messages[2]["content"], "Sunny, 72F");
    }

    #[test]
    fn test_disable_parallel_tool_use_inversion() {
        let input_true = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
            "max_tokens": 100,
            "disable_parallel_tool_use": true
        });
        let r_true = claude_to_openai_request(&input_true).unwrap();
        assert_eq!(r_true["parallel_tool_calls"], false);

        let input_false = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
            "max_tokens": 100,
            "disable_parallel_tool_use": false
        });
        let r_false = claude_to_openai_request(&input_false).unwrap();
        assert_eq!(r_false["parallel_tool_calls"], true);
    }

    #[test]
    fn test_thinking_budget_tokens_to_reasoning_effort() {
        let cases = [
            (1000u64, "low"),
            (1280, "low"),
            (1699, "low"),
            (1700, "medium"),
            (2048, "medium"),
            (2999, "medium"),
            (3000, "high"),
            (4096, "high"),
        ];
        for (budget, expected) in cases {
            let input = json!({
                "model": "claude-sonnet-4",
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
                "max_tokens": 100,
                "thinking": {"type": "enabled", "budget_tokens": budget}
            });
            let r = claude_to_openai_request(&input).unwrap();
            assert_eq!(r["reasoning_effort"], expected, "budget={}", budget);
        }
    }

    #[test]
    fn test_tools_input_schema_to_function_parameters() {
        let input = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
            "max_tokens": 100,
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {
                    "type": "object",
                    "properties": {"location": {"type": "string"}}
                }
            }]
        });
        let r = claude_to_openai_request(&input).unwrap();
        let tools = r["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(tools[0]["function"]["description"], "Get weather");
        assert_eq!(
            tools[0]["function"]["parameters"],
            json!({
                "type": "object",
                "properties": {"location": {"type": "string"}}
            })
        );
    }

    #[test]
    fn test_system_string_passthrough() {
        // Claude also accepts system as a plain string
        let input = json!({
            "model": "claude-sonnet-4",
            "system": "You are helpful",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hello"}]}
            ],
            "max_tokens": 100
        });
        let r = claude_to_openai_request(&input).unwrap();
        let messages = r["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful");
    }

    #[test]
    fn test_multiple_system_blocks_concatenated() {
        let input = json!({
            "model": "claude-sonnet-4",
            "system": [
                {"type": "text", "text": "Rule 1. "},
                {"type": "text", "text": "Rule 2."}
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
            "max_tokens": 100
        });
        let r = claude_to_openai_request(&input).unwrap();
        let messages = r["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Rule 1. Rule 2.");
    }

    #[test]
    fn test_temperature_top_p_stream_passthrough() {
        let input = json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
            "max_tokens": 100,
            "temperature": 0.7,
            "top_p": 0.9,
            "stream": true
        });
        let r = claude_to_openai_request(&input).unwrap();
        assert_eq!(r["temperature"], 0.7);
        assert_eq!(r["top_p"], 0.9);
        assert_eq!(r["stream"], true);
    }

    #[test]
    fn test_assistant_with_only_tool_use() {
        // Assistant message with only tool_use blocks → content=null
        let input = json!({
            "model": "claude-sonnet-4",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hi"}]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "fn", "input": {}}
                ]}
            ],
            "max_tokens": 100
        });
        let r = claude_to_openai_request(&input).unwrap();
        let messages = r["messages"].as_array().unwrap();
        assert_eq!(messages[1]["role"], "assistant");
        assert!(messages[1]["content"].is_null());
        assert!(messages[1].get("tool_calls").is_some());
    }

    // ---- openai_to_claude_response tests ----

    #[test]
    fn test_openai_response_to_claude_text_only() {
        let input = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let r = openai_to_claude_response(&input).unwrap();
        assert_eq!(r["id"], "chatcmpl-123");
        assert_eq!(r["type"], "message");
        assert_eq!(r["role"], "assistant");
        assert_eq!(r["model"], "gpt-4");
        let content = r["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello!");
        assert_eq!(r["stop_reason"], "end_turn");
        assert_eq!(r["stop_sequence"], json!(null));
        assert_eq!(r["usage"]["input_tokens"], 10);
        assert_eq!(r["usage"]["output_tokens"], 5);
    }

    #[test]
    fn test_openai_response_to_claude_with_tool_calls() {
        let input = json!({
            "id": "chatcmpl-456",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Let me check.",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"SF\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 15, "completion_tokens": 20, "total_tokens": 35}
        });
        let r = openai_to_claude_response(&input).unwrap();
        let content = r["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Let me check.");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "call_123");
        assert_eq!(content[1]["name"], "get_weather");
        assert_eq!(content[1]["input"], json!({"location": "SF"}));
        assert_eq!(r["stop_reason"], "tool_use");
        assert_eq!(r["usage"]["input_tokens"], 15);
        assert_eq!(r["usage"]["output_tokens"], 20);
    }

    #[test]
    fn test_finish_reason_mapping() {
        let cases = [
            ("stop", "end_turn"),
            ("tool_calls", "tool_use"),
            ("length", "max_tokens"),
            ("content_filter", "end_turn"),
        ];
        for (openai_reason, claude_reason) in cases {
            let input = json!({
                "id": "x",
                "model": "m",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": openai_reason
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            });
            let r = openai_to_claude_response(&input).unwrap();
            assert_eq!(
                r["stop_reason"],
                claude_reason,
                "openai_reason={}",
                openai_reason
            );
        }
    }

    #[test]
    fn test_openai_response_empty_content() {
        // Empty content string → still produce a text block (Claude expects content)
        let input = json!({
            "id": "x",
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": ""},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let r = openai_to_claude_response(&input).unwrap();
        let content = r["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
    }

    #[test]
    fn test_openai_response_only_tool_calls_no_content() {
        // content=null with tool_calls → only tool_use blocks
        let input = json!({
            "id": "x",
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "fn", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let r = openai_to_claude_response(&input).unwrap();
        let content = r["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_use");
    }
}

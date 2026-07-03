use chennix_common::ProxyResult;
use serde_json::{json, Value};

/// Convert an OpenAI `/v1/chat/completions` request body to Claude `/v1/messages` format.
///
/// Key transformations:
/// - Extract `messages[].role=system` → merge into `system` field (array of text blocks)
/// - Remaining messages: ensure user/assistant strict alternation (merge consecutive same-role)
/// - If first message is not user, prepend a placeholder `{"role":"user","content":"."}`
/// - `content` string → array of `{type:"text", text:"..."}`
/// - `tool_calls` → `tool_use` content blocks
/// - `role=tool` messages → `tool_result` content blocks (role becomes `user`)
/// - `tools[].function` → `tools[].input_schema`
/// - `parallel_tool_calls` → `disable_parallel_tool_use` (boolean inverted)
/// - `response_format` → append instruction to system prompt
/// - `reasoning_effort` → `thinking: {type:"enabled", budget_tokens: N}`
/// - `max_tokens` defaults to 4096 if missing
/// - Drops `stream`, `n`, `presence_penalty`, `frequency_penalty`, `logprobs`, `top_logprobs`
pub fn openai_to_claude_request(body: &Value) -> ProxyResult<Value> {
    let mut out = serde_json::Map::new();

    // model — pass through (caller will replace with upstream_model_name)
    if let Some(model) = body.get("model") {
        out.insert("model".to_string(), model.clone());
    }

    // 1. Walk messages: extract system texts, collect non-system as (role, content_blocks)
    let mut system_texts: Vec<String> = Vec::new();
    let mut pending: Vec<(String, Vec<Value>)> = Vec::new();

    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            let role = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string();

            if role == "system" {
                // Merge content (string or array-of-text) into system_texts
                if let Some(content) = msg.get("content") {
                    if let Some(s) = content.as_str() {
                        if !s.is_empty() {
                            system_texts.push(s.to_string());
                        }
                    } else if let Some(arr) = content.as_array() {
                        for part in arr {
                            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                    system_texts.push(t.to_string());
                                }
                            }
                        }
                    }
                }
                continue;
            }

            // role=tool → convert to user message with tool_result block
            if role == "tool" {
                let tool_use_id = msg.get("tool_call_id").cloned().unwrap_or(json!(""));
                let content_val = match msg.get("content") {
                    Some(Value::Null) | None => json!(""),
                    Some(v) => v.clone(),
                };
                pending.push((
                    "user".to_string(),
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content_val
                    })],
                ));
                continue;
            }

            // user / assistant: build content blocks
            let mut blocks: Vec<Value> = Vec::new();

            if let Some(content) = msg.get("content") {
                if let Some(s) = content.as_str() {
                    if !s.is_empty() {
                        blocks.push(json!({"type": "text", "text": s}));
                    }
                } else if let Some(arr) = content.as_array() {
                    for part in arr {
                        match part.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                blocks.push(json!({
                                    "type": "text",
                                    "text": part.get("text").cloned().unwrap_or(json!(""))
                                }));
                            }
                            // Pass through other content types (e.g. image_url) as-is
                            _ => blocks.push(part.clone()),
                        }
                    }
                }
            }

            // tool_calls (assistant) → tool_use blocks
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tool_calls {
                    let id = tc.get("id").cloned().unwrap_or(json!(""));
                    let empty_obj = json!({});
                    let function = tc.get("function").unwrap_or(&empty_obj);
                    let name = function.get("name").cloned().unwrap_or(json!(""));
                    let arguments_str = function
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}");
                    let input: Value = serde_json::from_str(arguments_str).unwrap_or(json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                }
            }

            pending.push((role, blocks));
        }
    }

    // 2. Merge consecutive same-role messages
    let mut merged: Vec<(String, Vec<Value>)> = Vec::new();
    for (role, blocks) in pending {
        if let Some(last) = merged.last_mut() {
            if last.0 == role {
                last.1.extend(blocks);
                continue;
            }
        }
        merged.push((role, blocks));
    }

    // 3. If first message is not user (or empty), prepend placeholder
    if merged.first().map(|(r, _)| r.as_str()) != Some("user") {
        merged.insert(0, ("user".to_string(), vec![json!({"type": "text", "text": "."})]));
    }

    // 4. Build messages array
    let messages_array: Vec<Value> = merged
        .into_iter()
        .map(|(role, blocks)| json!({"role": role, "content": blocks}))
        .collect();
    out.insert("messages".to_string(), Value::Array(messages_array));

    // 5. response_format → append instruction to system prompt
    if let Some(rf) = body.get("response_format") {
        let rf_type = rf.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match rf_type {
            "json_object" => {
                system_texts
                    .push("Please respond with a valid JSON object.".to_string());
            }
            "json_schema" => {
                if let Some(schema) = rf.get("json_schema").and_then(|s| s.get("schema")) {
                    system_texts.push(format!(
                        "Please respond with a JSON object matching this schema: {}",
                        schema
                    ));
                } else {
                    system_texts
                        .push("Please respond with a valid JSON object.".to_string());
                }
            }
            _ => {}
        }
    }

    // 6. Build system field (only if non-empty)
    if !system_texts.is_empty() {
        let system_array: Vec<Value> = system_texts
            .into_iter()
            .map(|t| json!({"type": "text", "text": t}))
            .collect();
        out.insert("system".to_string(), Value::Array(system_array));
    }

    // 7. max_tokens (required for Claude, default 4096)
    let max_tokens = body.get("max_tokens").and_then(|m| m.as_u64()).unwrap_or(4096);
    out.insert("max_tokens".to_string(), json!(max_tokens));

    // 8. temperature, top_p — pass through if present
    if let Some(t) = body.get("temperature") {
        out.insert("temperature".to_string(), t.clone());
    }
    if let Some(t) = body.get("top_p") {
        out.insert("top_p".to_string(), t.clone());
    }

    // 9. tools conversion: tools[].function → tools[].{name, description, input_schema}
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let claude_tools: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                let name = func.get("name").cloned().unwrap_or(json!(""));
                let input_schema = func.get("parameters").cloned().unwrap_or(json!({}));
                let mut t = serde_json::Map::new();
                t.insert("name".to_string(), name);
                if let Some(desc) = func.get("description") {
                    t.insert("description".to_string(), desc.clone());
                }
                t.insert("input_schema".to_string(), input_schema);
                Some(Value::Object(t))
            })
            .collect();
        if !claude_tools.is_empty() {
            out.insert("tools".to_string(), Value::Array(claude_tools));
        }
    }

    // 10. parallel_tool_calls → disable_parallel_tool_use (inverted)
    if let Some(ptc) = body.get("parallel_tool_calls").and_then(|p| p.as_bool()) {
        out.insert("disable_parallel_tool_use".to_string(), json!(!ptc));
    }

    // 11. reasoning_effort → thinking.budget_tokens
    if let Some(effort) = body.get("reasoning_effort").and_then(|e| e.as_str()) {
        let budget: u64 = match effort {
            "low" => 16000,
            "medium" => 32000,
            "high" => 64000,
            _ => 32000,
        };
        out.insert(
            "thinking".to_string(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
    }

    // Dropped (intentionally not copied): stream, n, presence_penalty,
    // frequency_penalty, logprobs, top_logprobs, tool_choice, seed, user

    Ok(Value::Object(out))
}

/// Convert a Claude `/v1/messages` response back to OpenAI `/v1/chat/completions` format.
///
/// Key transformations:
/// - `content[]` text blocks → `choices[0].message.content` (concatenated string)
/// - `content[].type=tool_use` → `choices[0].message.tool_calls` (arguments is JSON string)
/// - `stop_reason` → `finish_reason`: end_turn→stop, tool_use→tool_calls,
///   max_tokens→length, stop_sequence→stop
/// - `usage.input_tokens` → `usage.prompt_tokens`
/// - `usage.output_tokens` → `usage.completion_tokens`
/// - `usage.input_tokens + usage.output_tokens` → `usage.total_tokens`
/// - Wraps as `{id, object:"chat.completion", created, model, choices, usage}`
pub fn claude_to_openai_response(body: &Value) -> ProxyResult<Value> {
    let id = body.get("id").cloned().unwrap_or(json!(""));
    let model = body.get("model").cloned().unwrap_or(json!(""));

    // Build message: concatenate text blocks into content string, collect tool_use blocks
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
        for block in content {
            match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
                "tool_use" => {
                    let tc_id = block.get("id").cloned().unwrap_or(json!(""));
                    let name = block.get("name").cloned().unwrap_or(json!(""));
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    // OpenAI arguments is a JSON string; Claude input is a JSON object
                    let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(json!({
                        "id": tc_id,
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
    }

    let content_str = text_parts.join("");

    // stop_reason → finish_reason
    let stop_reason = body.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("");
    let finish_reason = match stop_reason {
        "end_turn" => "stop",
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        "stop_sequence" => "stop",
        _ => "stop",
    };

    // usage mapping
    let empty_obj = json!({});
    let usage_obj = body.get("usage").unwrap_or(&empty_obj);
    let input_tokens = usage_obj
        .get("input_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = usage_obj
        .get("output_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let total_tokens = input_tokens + output_tokens;

    // Build message object
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), json!(content_str));
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }

    let choices = vec![json!({
        "index": 0,
        "message": Value::Object(message),
        "finish_reason": finish_reason
    })];

    // created timestamp (unix seconds) — use std::time to avoid extra dep
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut out = serde_json::Map::new();
    out.insert("id".to_string(), id);
    out.insert("object".to_string(), json!("chat.completion"));
    out.insert("created".to_string(), json!(created));
    out.insert("model".to_string(), model);
    out.insert("choices".to_string(), Value::Array(choices));
    out.insert(
        "usage".to_string(),
        json!({
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": total_tokens
        }),
    );

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- openai_to_claude_request tests ----

    #[test]
    fn test_simple_text_message() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there"}
            ],
            "max_tokens": 100
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(r["model"], "gpt-4");
        assert_eq!(r["max_tokens"], 100);
        assert_eq!(
            r["system"],
            json!([{"type": "text", "text": "You are helpful"}])
        );
        assert_eq!(r["messages"].as_array().unwrap().len(), 2);
        assert_eq!(r["messages"][0]["role"], "user");
        assert_eq!(
            r["messages"][0]["content"],
            json!([{"type": "text", "text": "Hello"}])
        );
        assert_eq!(r["messages"][1]["role"], "assistant");
        assert_eq!(
            r["messages"][1]["content"],
            json!([{"type": "text", "text": "Hi there"}])
        );
    }

    #[test]
    fn test_system_extraction_and_merging() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "Rule 1"},
                {"role": "system", "content": "Rule 2"},
                {"role": "user", "content": "Hi"}
            ],
            "max_tokens": 100
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(
            r["system"],
            json!([
                {"type": "text", "text": "Rule 1"},
                {"type": "text", "text": "Rule 2"}
            ])
        );
        // Only the user message should remain in messages
        assert_eq!(r["messages"].as_array().unwrap().len(), 1);
        assert_eq!(r["messages"][0]["role"], "user");
    }

    #[test]
    fn test_consecutive_same_role_merging() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "user", "content": "How are you?"},
                {"role": "assistant", "content": "I'm good"},
                {"role": "assistant", "content": "Thanks for asking"}
            ],
            "max_tokens": 100
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(r["messages"].as_array().unwrap().len(), 2);
        assert_eq!(r["messages"][0]["role"], "user");
        assert_eq!(
            r["messages"][0]["content"],
            json!([
                {"type": "text", "text": "Hello"},
                {"type": "text", "text": "How are you?"}
            ])
        );
        assert_eq!(r["messages"][1]["role"], "assistant");
        assert_eq!(
            r["messages"][1]["content"],
            json!([
                {"type": "text", "text": "I'm good"},
                {"type": "text", "text": "Thanks for asking"}
            ])
        );
    }

    #[test]
    fn test_first_message_not_user_placeholder() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": "Hi"}
            ],
            "max_tokens": 100
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(r["messages"].as_array().unwrap().len(), 2);
        assert_eq!(r["messages"][0]["role"], "user");
        assert_eq!(
            r["messages"][0]["content"],
            json!([{"type": "text", "text": "."}])
        );
        assert_eq!(r["messages"][1]["role"], "assistant");
    }

    #[test]
    fn test_tool_calls_conversion() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather?"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_123",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"location\":\"SF\"}"
                            }
                        }
                    ]
                }
            ],
            "max_tokens": 100
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(r["messages"][1]["role"], "assistant");
        let content = r["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "call_123");
        assert_eq!(content[0]["name"], "get_weather");
        assert_eq!(content[0]["input"], json!({"location": "SF"}));
    }

    #[test]
    fn test_tool_results_conversion() {
        let input = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "What's the weather?"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_123",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"location\":\"SF\"}"
                            }
                        }
                    ]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_123",
                    "content": "Sunny, 72F"
                }
            ],
            "max_tokens": 100
        });
        let r = openai_to_claude_request(&input).unwrap();
        // tool message becomes user with tool_result content
        assert_eq!(r["messages"][2]["role"], "user");
        let content = r["messages"][2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "call_123");
        assert_eq!(content[0]["content"], "Sunny, 72F");
    }

    #[test]
    fn test_parallel_tool_calls_inversion() {
        let input_true = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "parallel_tool_calls": true
        });
        let r_true = openai_to_claude_request(&input_true).unwrap();
        assert_eq!(r_true["disable_parallel_tool_use"], false);

        let input_false = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "parallel_tool_calls": false
        });
        let r_false = openai_to_claude_request(&input_false).unwrap();
        assert_eq!(r_false["disable_parallel_tool_use"], true);
    }

    #[test]
    fn test_response_format_to_system_prompt() {
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "response_format": {"type": "json_object"}
        });
        let r = openai_to_claude_request(&input).unwrap();
        let system = r["system"].as_array().unwrap();
        assert!(!system.is_empty());
        let last = system.last().unwrap();
        assert_eq!(last["type"], "text");
        assert!(last["text"].as_str().unwrap().to_lowercase().contains("json"));
    }

    #[test]
    fn test_reasoning_effort_to_thinking() {
        let cases = [("low", 16000u64), ("medium", 32000), ("high", 64000)];
        for (effort, budget) in cases {
            let input = json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "Hi"}],
                "max_tokens": 100,
                "reasoning_effort": effort
            });
            let r = openai_to_claude_request(&input).unwrap();
            assert_eq!(r["thinking"]["type"], "enabled", "effort={}", effort);
            assert_eq!(r["thinking"]["budget_tokens"], budget, "effort={}", effort);
        }
    }

    #[test]
    fn test_max_tokens_default_and_drop_unsupported() {
        // max_tokens missing → default 4096
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "stream": true,
            "n": 1,
            "presence_penalty": 0.5,
            "frequency_penalty": 0.5,
            "logprobs": true,
            "top_logprobs": 5
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(r["max_tokens"], 4096);
        // Dropped fields must not appear
        assert!(r.get("stream").is_none());
        assert!(r.get("n").is_none());
        assert!(r.get("presence_penalty").is_none());
        assert!(r.get("frequency_penalty").is_none());
        assert!(r.get("logprobs").is_none());
        assert!(r.get("top_logprobs").is_none());
    }

    #[test]
    fn test_tools_conversion() {
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get weather",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "location": {"type": "string"}
                            }
                        }
                    }
                }
            ]
        });
        let r = openai_to_claude_request(&input).unwrap();
        let tools = r["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "Get weather");
        assert_eq!(
            tools[0]["input_schema"],
            json!({
                "type": "object",
                "properties": {"location": {"type": "string"}}
            })
        );
        // Should NOT have the "type":"function" wrapper
        assert!(tools[0].get("type").is_none());
        assert!(tools[0].get("function").is_none());
    }

    #[test]
    fn test_temperature_top_p_passthrough() {
        let input = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 100,
            "temperature": 0.7,
            "top_p": 0.9
        });
        let r = openai_to_claude_request(&input).unwrap();
        assert_eq!(r["temperature"], 0.7);
        assert_eq!(r["top_p"], 0.9);
    }

    // ---- claude_to_openai_response tests ----

    #[test]
    fn test_claude_response_to_openai_text_only() {
        let input = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4",
            "content": [
                {"type": "text", "text": "Hello!"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let r = claude_to_openai_response(&input).unwrap();
        assert_eq!(r["id"], "msg_123");
        assert_eq!(r["object"], "chat.completion");
        assert_eq!(r["model"], "claude-sonnet-4");
        assert!(r.get("created").is_some());
        assert_eq!(r["choices"].as_array().unwrap().len(), 1);
        assert_eq!(r["choices"][0]["index"], 0);
        assert_eq!(r["choices"][0]["message"]["role"], "assistant");
        assert_eq!(r["choices"][0]["message"]["content"], "Hello!");
        assert!(r["choices"][0]["message"].get("tool_calls").is_none());
        assert_eq!(r["choices"][0]["finish_reason"], "stop");
        assert_eq!(r["usage"]["prompt_tokens"], 10);
        assert_eq!(r["usage"]["completion_tokens"], 5);
        assert_eq!(r["usage"]["total_tokens"], 15);
    }

    #[test]
    fn test_claude_response_to_openai_with_tool_use() {
        let input = json!({
            "id": "msg_456",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4",
            "content": [
                {"type": "text", "text": "Let me check the weather."},
                {
                    "type": "tool_use",
                    "id": "toolu_abc",
                    "name": "get_weather",
                    "input": {"location": "SF"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 15, "output_tokens": 20}
        });
        let r = claude_to_openai_response(&input).unwrap();
        assert_eq!(r["choices"][0]["message"]["content"], "Let me check the weather.");
        let tool_calls = r["choices"][0]["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "toolu_abc");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
        // arguments must be a JSON string
        assert!(tool_calls[0]["function"]["arguments"].is_string());
        let args: Value =
            serde_json::from_str(tool_calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({"location": "SF"}));
        assert_eq!(r["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(r["usage"]["total_tokens"], 35);
    }

    #[test]
    fn test_claude_response_stop_reason_mappings() {
        let cases = [
            ("end_turn", "stop"),
            ("tool_use", "tool_calls"),
            ("max_tokens", "length"),
            ("stop_sequence", "stop"),
        ];
        for (claude_reason, openai_reason) in cases {
            let input = json!({
                "id": "msg_x",
                "model": "m",
                "content": [{"type": "text", "text": "hi"}],
                "stop_reason": claude_reason,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            });
            let r = claude_to_openai_response(&input).unwrap();
            assert_eq!(
                r["choices"][0]["finish_reason"],
                openai_reason,
                "claude_reason={}",
                claude_reason
            );
        }
    }

    #[test]
    fn test_claude_response_multiple_text_blocks_concat() {
        let input = json!({
            "id": "msg_multi",
            "model": "m",
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "World!"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let r = claude_to_openai_response(&input).unwrap();
        assert_eq!(r["choices"][0]["message"]["content"], "Hello World!");
    }
}

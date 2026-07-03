use bytes::Bytes;
use chennix_common::{ProxyError, ProxyResult, Usage};
use serde_json::{json, Value};

// ============================================================================
// OpenAI SSE → Claude SSE
// ============================================================================

/// Streaming state machine that converts OpenAI chat completion SSE chunks
/// (`data: {json}\n\n`) into Claude message SSE events
/// (`event: X\ndata: {json}\n\n`).
///
/// State tracked:
/// - `content_block_index` — current Claude content block index (0=text, 1+=tool_use)
/// - `tool_call_index` — number of tool calls seen
/// - `started` — whether `message_start` has been emitted
/// - `deferred_usage` — buffered usage (from inline or separate usage chunk)
/// - `current_tool_id` — id of the tool currently being streamed
pub struct OpenaiToClaudeStreamState {
    pub content_block_index: i32,
    pub tool_call_index: i32,
    pub started: bool,
    pub deferred_usage: Option<Usage>,
    pub current_tool_id: Option<String>,
    // internal bookkeeping
    current_oai_tool_index: i32,
    message_id: String,
    model: String,
    finished: bool,
    deferred_stop_reason: Option<String>,
    message_stopped: bool,
}

impl Default for OpenaiToClaudeStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenaiToClaudeStreamState {
    pub fn new() -> Self {
        Self {
            content_block_index: 0,
            tool_call_index: 0,
            started: false,
            deferred_usage: None,
            current_tool_id: None,
            current_oai_tool_index: -1,
            message_id: String::new(),
            model: String::new(),
            finished: false,
            deferred_stop_reason: None,
            message_stopped: false,
        }
    }

    /// Process a raw SSE chunk from OpenAI (may contain multiple `data:` lines).
    /// Returns a vector of Claude SSE event bytes to send to the client.
    pub fn process_chunk(&mut self, chunk: &Bytes) -> ProxyResult<Vec<Bytes>> {
        let mut outputs: Vec<Bytes> = Vec::new();
        let text = std::str::from_utf8(chunk)
            .map_err(|e| ProxyError::Translator(format!("invalid utf8 in SSE chunk: {}", e)))?;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }
            let data_str = line["data:".len()..].trim();
            if data_str == "[DONE]" {
                // If finish_reason was seen but message_stop not yet emitted (e.g. no
                // separate usage chunk arrived), emit message_delta + message_stop now.
                if self.finished && !self.message_stopped {
                    outputs.push(self.make_message_delta());
                    outputs.push(Self::make_event("message_stop", &json!({"type":"message_stop"})));
                    self.message_stopped = true;
                }
                continue;
            }
            let v: Value = serde_json::from_str(data_str)
                .map_err(|e| ProxyError::Translator(format!("invalid JSON in SSE data: {}", e)))?;
            outputs.extend(self.process_json(&v)?);
        }

        Ok(outputs)
    }

    fn process_json(&mut self, v: &Value) -> ProxyResult<Vec<Bytes>> {
        let mut out: Vec<Bytes> = Vec::new();

        // Extract id and model from the first chunk
        if !self.started {
            if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                self.message_id = id.to_string();
            }
            if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
                self.model = model.to_string();
            }
        }

        // Buffer usage if present (OpenAI sends it in a separate chunk after finish_reason
        // when stream_options.include_usage is set, or inline in some providers).
        if let Some(usage) = v.get("usage") {
            if !usage.is_null() {
                let prompt = usage.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
                let completion = usage
                    .get("completion_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
                self.deferred_usage = Some(Usage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: prompt + completion,
                });
            }
        }

        // No choices array (or empty) → usage-only chunk (or malformed).
        // If we already saw finish_reason, this is the deferred usage chunk →
        // emit message_delta + message_stop.
        let choices = match v.get("choices").and_then(|c| c.as_array()) {
            Some(c) if !c.is_empty() => c,
            _ => {
                if self.finished && !self.message_stopped {
                    out.push(self.make_message_delta());
                    out.push(Self::make_event("message_stop", &json!({"type":"message_stop"})));
                    self.message_stopped = true;
                }
                return Ok(out);
            }
        };

        // On the very first chunk, emit message_start + content_block_start (text, index 0).
        if !self.started {
            out.push(self.make_message_start());
            out.push(Self::make_content_block_start_text(0));
            self.started = true;
            self.content_block_index = 0;
        }

        let choice = match choices.first() {
            Some(c) => c,
            None => return Ok(out),
        };

        let delta = choice.get("delta").unwrap_or(&Value::Null);

        // Process tool_calls deltas
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                let tc_index = tc.get("index").and_then(|i| i.as_i64()).unwrap_or(0) as i32;
                let tc_id = tc.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());

                // A new tool call (different OpenAI index) → close current block, open tool_use
                if tc_index != self.current_oai_tool_index {
                    out.push(Self::make_content_block_stop(self.content_block_index));
                    self.content_block_index += 1;

                    let id = tc_id.clone().unwrap_or_default();
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push(Self::make_content_block_start_tool_use(
                        self.content_block_index,
                        &id,
                        &name,
                    ));

                    self.current_oai_tool_index = tc_index;
                    self.current_tool_id = Some(id);
                    self.tool_call_index += 1;
                }

                // Emit arguments as input_json_delta (skip empty fragments)
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    if !args.is_empty() {
                        out.push(Self::make_input_json_delta(self.content_block_index, args));
                    }
                }
            }
        }

        // Process text content
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                out.push(Self::make_text_delta(self.content_block_index, content));
            }
        }

        // Process finish_reason
        // 对齐 new-api StreamResponseOpenAI2Claude / CLIProxyAPI ConvertOpenAIResponseToClaude：
        // finish_reason 到达时只关闭 content_block，延迟发 message_delta/message_stop，
        // 等后续 usage-only chunk 到达后再补发（OpenAI 的 usage 常在 finish 之后的独立 chunk）。
        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            // Close the current content block
            out.push(Self::make_content_block_stop(self.content_block_index));

            let stop_reason = match fr {
                "stop" => "end_turn",
                "tool_calls" => "tool_use",
                "length" => "max_tokens",
                "content_filter" => "end_turn",
                _ => "end_turn",
            };

            self.finished = true;
            self.deferred_stop_reason = Some(stop_reason.to_string());

            // 若 usage 已就绪（inline 或之前累积），立即发 message_delta + message_stop。
            // 否则延迟到 usage-only chunk 或 [DONE] 兜底。
            if self.deferred_usage.is_some() {
                out.push(self.make_message_delta());
                out.push(Self::make_event("message_stop", &json!({"type":"message_stop"})));
                self.message_stopped = true;
            }
            // 若 deferred_usage 为 None，不发 message_delta/message_stop，
            // 等待 usage-only chunk（process_json 开头的 choices==None 分支处理）
            // 或 [DONE] 兜底（process_chunk 的 [DONE] 分支处理）。
        }

        Ok(out)
    }

    // ---- Claude SSE event constructors ----

    fn make_message_start(&self) -> Bytes {
        Self::make_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }),
        )
    }

    fn make_content_block_start_text(index: i32) -> Bytes {
        Self::make_event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "text", "text": ""}
            }),
        )
    }

    fn make_content_block_start_tool_use(index: i32, id: &str, name: &str) -> Bytes {
        Self::make_event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
            }),
        )
    }

    fn make_text_delta(index: i32, text: &str) -> Bytes {
        Self::make_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "text_delta", "text": text}
            }),
        )
    }

    fn make_input_json_delta(index: i32, partial_json: &str) -> Bytes {
        Self::make_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "input_json_delta", "partial_json": partial_json}
            }),
        )
    }

    fn make_content_block_stop(index: i32) -> Bytes {
        Self::make_event(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        )
    }

    fn make_message_delta(&self) -> Bytes {
        let stop_reason = self.deferred_stop_reason.as_deref().unwrap_or("end_turn");
        let output_tokens = self
            .deferred_usage
            .as_ref()
            .map(|u| u.completion_tokens)
            .unwrap_or(0);
        Self::make_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"output_tokens": output_tokens}
            }),
        )
    }

    fn make_event(event_type: &str, data: &Value) -> Bytes {
        Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
    }
}

// ============================================================================
// Claude SSE → OpenAI SSE
// ============================================================================

/// Streaming state machine that converts Claude message SSE events
/// into OpenAI chat completion SSE chunks.
///
/// State tracked:
/// - `started` — whether the first `role=assistant` chunk has been emitted
/// - `current_block_type` — `Some("text")` or `Some("tool_use")`
/// - `current_block_index` — Claude content block index
/// - `tool_call_index` — OpenAI tool_call index (separate from Claude's block index)
/// - `deferred_usage` — buffered usage from `message_delta`
/// - `input_tokens` — from `message_start`
pub struct ClaudeToOpenaiStreamState {
    pub started: bool,
    pub current_block_type: Option<String>,
    pub current_block_index: i32,
    pub tool_call_index: i32,
    pub deferred_usage: Option<Usage>,
    pub input_tokens: u64,
    // internal bookkeeping
    deferred_stop_reason: Option<String>,
    deferred_output_tokens: u64,
    current_oai_tool_index: Option<i32>,
    message_id: String,
    model: String,
    created: u64,
}

impl Default for ClaudeToOpenaiStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeToOpenaiStreamState {
    pub fn new() -> Self {
        Self {
            started: false,
            current_block_type: None,
            current_block_index: 0,
            tool_call_index: 0,
            deferred_usage: None,
            input_tokens: 0,
            deferred_stop_reason: None,
            deferred_output_tokens: 0,
            current_oai_tool_index: None,
            message_id: String::new(),
            model: String::new(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// Process a Claude SSE event (event type + data bytes).
    /// Returns a vector of OpenAI SSE chunk bytes to send to the client.
    pub fn process_event(&mut self, event: &str, data: &Bytes) -> ProxyResult<Vec<Bytes>> {
        let mut out: Vec<Bytes> = Vec::new();

        let data_str = std::str::from_utf8(data)
            .map_err(|e| ProxyError::Translator(format!("invalid utf8 in SSE data: {}", e)))?;
        let v: Value = serde_json::from_str(data_str)
            .map_err(|e| ProxyError::Translator(format!("invalid JSON in SSE data: {}", e)))?;

        match event {
            "message_start" => {
                if let Some(msg) = v.get("message") {
                    if let Some(id) = msg.get("id").and_then(|i| i.as_str()) {
                        self.message_id = id.to_string();
                    }
                    if let Some(model) = msg.get("model").and_then(|m| m.as_str()) {
                        self.model = model.to_string();
                    }
                    if let Some(usage) = msg.get("usage") {
                        self.input_tokens =
                            usage.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
                    }
                }
                // No output yet — wait for first content.
            }

            "content_block_start" => {
                let block = v.get("content_block").unwrap_or(&Value::Null);
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let index = v.get("index").and_then(|i| i.as_i64()).unwrap_or(0) as i32;
                self.current_block_index = index;

                if block_type == "text" {
                    if !self.started {
                        out.push(self.make_role_chunk());
                        self.started = true;
                    }
                    self.current_block_type = Some("text".to_string());
                } else if block_type == "tool_use" {
                    if !self.started {
                        out.push(self.make_role_chunk());
                        self.started = true;
                    }
                    let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    out.push(self.make_tool_call_start_chunk(self.tool_call_index, id, name));
                    self.current_oai_tool_index = Some(self.tool_call_index);
                    self.tool_call_index += 1;
                    self.current_block_type = Some("tool_use".to_string());
                }
            }

            "content_block_delta" => {
                let delta = v.get("delta").unwrap_or(&Value::Null);
                let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");

                if delta_type == "text_delta" {
                    if !self.started {
                        out.push(self.make_role_chunk());
                        self.started = true;
                    }
                    let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    out.push(self.make_content_chunk(text));
                } else if delta_type == "input_json_delta" {
                    let partial = delta
                        .get("partial_json")
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    if let Some(idx) = self.current_oai_tool_index {
                        out.push(self.make_tool_call_args_chunk(idx, partial));
                    }
                }
            }

            "content_block_stop" => {
                // No output — just state tracking.
            }

            "message_delta" => {
                if let Some(delta) = v.get("delta") {
                    if let Some(sr) = delta.get("stop_reason").and_then(|s| s.as_str()) {
                        self.deferred_stop_reason = Some(sr.to_string());
                    }
                }
                if let Some(usage) = v.get("usage") {
                    self.deferred_output_tokens =
                        usage.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
                    self.deferred_usage = Some(Usage {
                        prompt_tokens: self.input_tokens,
                        completion_tokens: self.deferred_output_tokens,
                        total_tokens: self.input_tokens + self.deferred_output_tokens,
                    });
                }
            }

            "message_stop" => {
                let stop_reason = self.deferred_stop_reason.as_deref().unwrap_or("end_turn");
                let finish_reason = match stop_reason {
                    "end_turn" => "stop",
                    "tool_use" => "tool_calls",
                    "max_tokens" => "length",
                    "stop_sequence" => "stop",
                    _ => "stop",
                };

                out.push(self.make_finish_chunk(finish_reason));

                // Emit usage chunk if we have any usage data
                if self.input_tokens > 0 || self.deferred_output_tokens > 0 {
                    out.push(self.make_usage_chunk(self.input_tokens, self.deferred_output_tokens));
                }

                out.push(Bytes::from("data: [DONE]\n\n"));
            }

            "ping" => {
                // Ignore — no output.
            }

            _ => {
                // Unknown event — ignore.
            }
        }

        Ok(out)
    }

    // ---- OpenAI SSE chunk constructors ----

    fn make_role_chunk(&self) -> Bytes {
        Self::make_chunk(&json!({
            "id": self.message_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
        }))
    }

    fn make_content_chunk(&self, text: &str) -> Bytes {
        Self::make_chunk(&json!({
            "id": self.message_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
        }))
    }

    fn make_tool_call_start_chunk(&self, index: i32, id: &str, name: &str) -> Bytes {
        Self::make_chunk(&json!({
            "id": self.message_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": index, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]},
                "finish_reason": null
            }]
        }))
    }

    fn make_tool_call_args_chunk(&self, index: i32, args: &str) -> Bytes {
        Self::make_chunk(&json!({
            "id": self.message_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": index, "function": {"arguments": args}}]},
                "finish_reason": null
            }]
        }))
    }

    fn make_finish_chunk(&self, finish_reason: &str) -> Bytes {
        Self::make_chunk(&json!({
            "id": self.message_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}]
        }))
    }

    fn make_usage_chunk(&self, prompt_tokens: u64, completion_tokens: u64) -> Bytes {
        Self::make_chunk(&json!({
            "id": self.message_id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens
            }
        }))
    }

    fn make_chunk(v: &Value) -> Bytes {
        Bytes::from(format!("data: {}\n\n", v))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // ---- helpers ----

    /// Parse Claude SSE output bytes into a list of (event_type, data_json) pairs.
    fn parse_claude_events(bytes: &[Bytes]) -> Vec<(String, Value)> {
        let mut events = Vec::new();
        for b in bytes {
            let s = std::str::from_utf8(b).unwrap();
            // format: "event: {type}\ndata: {json}\n\n"
            let mut event_type = String::new();
            let mut data_str = String::new();
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    event_type = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_str = rest.trim().to_string();
                }
            }
            let v: Value = serde_json::from_str(&data_str).unwrap_or(Value::Null);
            events.push((event_type, v));
        }
        events
    }

    /// Parse OpenAI SSE output bytes into a list of data JSON values
    /// (or a "[DONE]" marker as Value::String("[DONE]")).
    fn parse_openai_chunks(bytes: &[Bytes]) -> Vec<Value> {
        let mut chunks = Vec::new();
        for b in bytes {
            let s = std::str::from_utf8(b).unwrap();
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    let rest = rest.trim();
                    if rest == "[DONE]" {
                        chunks.push(Value::String("[DONE]".to_string()));
                    } else {
                        let v: Value = serde_json::from_str(rest).unwrap_or(Value::Null);
                        chunks.push(v);
                    }
                }
            }
        }
        chunks
    }

    fn oai_chunk(id: &str, model: &str, delta: Value, finish_reason: Option<&str>) -> String {
        let fr = match finish_reason {
            Some(r) => json!(r),
            None => json!(null),
        };
        format!(
            "data: {}\n\n",
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": model,
                "choices": [{"index": 0, "delta": delta, "finish_reason": fr}]
            })
        )
    }

    fn oai_usage_chunk(id: &str, model: &str, prompt: u64, completion: u64) -> String {
        format!(
            "data: {}\n\n",
            json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": model,
                "choices": [],
                "usage": {
                    "prompt_tokens": prompt,
                    "completion_tokens": completion,
                    "total_tokens": prompt + completion
                }
            })
        )
    }

    fn claude_event(event_type: &str, data: Value) -> (String, Bytes) {
        (event_type.to_string(), Bytes::from(format!("{}\n", json!(data))))
    }

    // ========================================================================
    // O2C Tests (OpenAI SSE → Claude SSE)
    // ========================================================================

    #[test]
    fn test_o2c_simple_text_stream() {
        let mut state = OpenaiToClaudeStreamState::new();

        // Chunk 1: role + content
        let chunk1 = Bytes::from(oai_chunk("chatcmpl-1", "gpt-4", json!({"role":"assistant","content":"Hello"}), None));
        let out1 = state.process_chunk(&chunk1).unwrap();
        // Chunk 2: more content
        let chunk2 = Bytes::from(oai_chunk("chatcmpl-1", "gpt-4", json!({"content":" world"}), None));
        let out2 = state.process_chunk(&chunk2).unwrap();
        // Chunk 3: finish_reason
        let chunk3 = Bytes::from(oai_chunk("chatcmpl-1", "gpt-4", json!({}), Some("stop")));
        let out3 = state.process_chunk(&chunk3).unwrap();
        // Chunk 4: [DONE]
        let chunk4 = Bytes::from("data: [DONE]\n\n");
        let out4 = state.process_chunk(&chunk4).unwrap();

        // Chunk 1 should produce: message_start, content_block_start (text), content_block_delta (text)
        let e1 = parse_claude_events(&out1);
        assert_eq!(e1.len(), 3);
        assert_eq!(e1[0].0, "message_start");
        assert_eq!(e1[0].1["type"], "message_start");
        assert_eq!(e1[0].1["message"]["id"], "chatcmpl-1");
        assert_eq!(e1[0].1["message"]["model"], "gpt-4");
        assert_eq!(e1[0].1["message"]["role"], "assistant");
        assert_eq!(e1[0].1["message"]["usage"]["input_tokens"], 0);
        assert_eq!(e1[0].1["message"]["usage"]["output_tokens"], 0);

        assert_eq!(e1[1].0, "content_block_start");
        assert_eq!(e1[1].1["index"], 0);
        assert_eq!(e1[1].1["content_block"]["type"], "text");

        assert_eq!(e1[2].0, "content_block_delta");
        assert_eq!(e1[2].1["index"], 0);
        assert_eq!(e1[2].1["delta"]["type"], "text_delta");
        assert_eq!(e1[2].1["delta"]["text"], "Hello");

        // Chunk 2 should produce: content_block_delta (text)
        let e2 = parse_claude_events(&out2);
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].0, "content_block_delta");
        assert_eq!(e2[0].1["delta"]["text"], " world");

        // Chunk 3 should produce: content_block_stop only (usage 未就绪，延迟 message_delta)
        let e3 = parse_claude_events(&out3);
        assert_eq!(e3.len(), 1);
        assert_eq!(e3[0].0, "content_block_stop");
        assert_eq!(e3[0].1["index"], 0);

        // Chunk 4 [DONE] should produce: message_delta (兜底, output_tokens=0) + message_stop
        let e4 = parse_claude_events(&out4);
        assert_eq!(e4.len(), 2);
        assert_eq!(e4[0].0, "message_delta");
        assert_eq!(e4[0].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(e4[0].1["delta"]["stop_sequence"], json!(null));
        assert_eq!(e4[1].0, "message_stop");
    }

    #[test]
    fn test_o2c_with_tool_calls() {
        let mut state = OpenaiToClaudeStreamState::new();

        // Chunk 1: text content
        let chunk1 = Bytes::from(oai_chunk("c1", "gpt-4", json!({"role":"assistant","content":"Let me check"}), None));
        let _ = state.process_chunk(&chunk1).unwrap();

        // Chunk 2: first tool_call (index 0, id + name + empty args)
        let chunk2 = Bytes::from(oai_chunk("c1", "gpt-4", json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_abc",
                "type": "function",
                "function": {"name": "get_weather", "arguments": ""}
            }]
        }), None));
        let out2 = state.process_chunk(&chunk2).unwrap();
        let e2 = parse_claude_events(&out2);
        // Expect: content_block_stop (close text), content_block_start (tool_use)
        assert_eq!(e2.len(), 2);
        assert_eq!(e2[0].0, "content_block_stop");
        assert_eq!(e2[0].1["index"], 0);
        assert_eq!(e2[1].0, "content_block_start");
        assert_eq!(e2[1].1["index"], 1);
        assert_eq!(e2[1].1["content_block"]["type"], "tool_use");
        assert_eq!(e2[1].1["content_block"]["id"], "call_abc");
        assert_eq!(e2[1].1["content_block"]["name"], "get_weather");
        assert_eq!(e2[1].1["content_block"]["input"], json!({}));

        // Chunk 3: tool_call arguments delta (same index 0)
        let chunk3 = Bytes::from(oai_chunk("c1", "gpt-4", json!({
            "tool_calls": [{
                "index": 0,
                "function": {"arguments": "{\"location\":\"SF\"}"}
            }]
        }), None));
        let out3 = state.process_chunk(&chunk3).unwrap();
        let e3 = parse_claude_events(&out3);
        assert_eq!(e3.len(), 1);
        assert_eq!(e3[0].0, "content_block_delta");
        assert_eq!(e3[0].1["index"], 1);
        assert_eq!(e3[0].1["delta"]["type"], "input_json_delta");
        assert_eq!(e3[0].1["delta"]["partial_json"], "{\"location\":\"SF\"}");

        // Chunk 4: finish_reason=tool_calls（无 usage → 只发 content_block_stop）
        let chunk4 = Bytes::from(oai_chunk("c1", "gpt-4", json!({}), Some("tool_calls")));
        let out4 = state.process_chunk(&chunk4).unwrap();
        let e4 = parse_claude_events(&out4);
        assert_eq!(e4.len(), 1);
        assert_eq!(e4[0].0, "content_block_stop");
        assert_eq!(e4[0].1["index"], 1);

        // [DONE] 兜底发 message_delta + message_stop
        let chunk5 = Bytes::from("data: [DONE]\n\n");
        let out5 = state.process_chunk(&chunk5).unwrap();
        let e5 = parse_claude_events(&out5);
        assert_eq!(e5.len(), 2);
        assert_eq!(e5[0].0, "message_delta");
        assert_eq!(e5[0].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(e5[1].0, "message_stop");
    }

    #[test]
    fn test_o2c_finish_reason_mapping() {
        let cases = [
            ("stop", "end_turn"),
            ("tool_calls", "tool_use"),
            ("length", "max_tokens"),
        ];
        for (oai_fr, claude_sr) in cases {
            let mut state = OpenaiToClaudeStreamState::new();
            // Single chunk with content + finish_reason
            let chunk = Bytes::from(oai_chunk("c1", "m", json!({"role":"assistant","content":"hi"}), Some(oai_fr)));
            let _ = state.process_chunk(&chunk).unwrap();
            // [DONE] 兜底发 message_delta
            let done = Bytes::from("data: [DONE]\n\n");
            let out = state.process_chunk(&done).unwrap();
            let events = parse_claude_events(&out);
            // Find the message_delta event
            let md = events.iter().find(|(t, _)| t == "message_delta").unwrap();
            assert_eq!(md.1["delta"]["stop_reason"], claude_sr, "oai_fr={}", oai_fr);
        }
    }

    #[test]
    fn test_o2c_usage_in_separate_chunk() {
        // With include_usage, OpenAI sends: content chunks → finish_reason chunk → usage chunk → [DONE]
        // 对齐 new-api：finish_reason 到达时若 usage 未就绪，延迟发 message_delta，
        // 等 usage-only chunk 到达后补发（带真实 output_tokens）。
        let mut state = OpenaiToClaudeStreamState::new();

        let chunk1 = Bytes::from(oai_chunk("c1", "gpt-4", json!({"role":"assistant","content":"Hi"}), None));
        let _ = state.process_chunk(&chunk1).unwrap();

        // finish_reason chunk（无 usage）→ 只发 content_block_stop，延迟 message_delta
        let chunk2 = Bytes::from(oai_chunk("c1", "gpt-4", json!({}), Some("stop")));
        let out2 = state.process_chunk(&chunk2).unwrap();
        let e2 = parse_claude_events(&out2);
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].0, "content_block_stop");

        // Usage-only chunk (choices=[]) → 补发 message_delta（带真实 output_tokens）+ message_stop
        let chunk3 = Bytes::from(oai_usage_chunk("c1", "gpt-4", 10, 5));
        let out3 = state.process_chunk(&chunk3).unwrap();
        let e3 = parse_claude_events(&out3);
        assert_eq!(e3.len(), 2);
        assert_eq!(e3[0].0, "message_delta");
        assert_eq!(e3[0].1["usage"]["output_tokens"], 5);
        assert_eq!(e3[0].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(e3[1].0, "message_stop");

        let chunk4 = Bytes::from("data: [DONE]\n\n");
        let out4 = state.process_chunk(&chunk4).unwrap();
        assert!(out4.is_empty());
    }

    #[test]
    fn test_o2c_done_without_usage() {
        // 若 usage 始终未到，[DONE] 兜底发 message_delta（output_tokens=0）+ message_stop。
        // 对齐 CLIProxyAPI 的 [DONE] 兜底策略。
        let mut state = OpenaiToClaudeStreamState::new();
        let chunk1 = Bytes::from(oai_chunk("c1", "gpt-4", json!({"role":"assistant","content":"Hi"}), Some("stop")));
        let out1 = state.process_chunk(&chunk1).unwrap();
        // finish_reason 无 usage → 不发 message_delta/message_stop
        // 输出：message_start + content_block_start + content_block_delta + content_block_stop
        let e1 = parse_claude_events(&out1);
        assert_eq!(e1.len(), 4);
        assert_eq!(e1.last().unwrap().0, "content_block_stop");
        // 确认没有 message_delta/message_stop
        assert!(e1.iter().all(|(t, _)| t != "message_delta" && t != "message_stop"));

        let chunk2 = Bytes::from("data: [DONE]\n\n");
        let out2 = state.process_chunk(&chunk2).unwrap();
        let e2 = parse_claude_events(&out2);
        // [DONE] 兜底：message_delta（output_tokens=0）+ message_stop
        assert_eq!(e2.len(), 2);
        assert_eq!(e2[0].0, "message_delta");
        assert_eq!(e2[0].1["usage"]["output_tokens"], 0);
        assert_eq!(e2[1].0, "message_stop");
    }

    // ========================================================================
    // C2O Tests (Claude SSE → OpenAI SSE)
    // ========================================================================

    #[test]
    fn test_c2o_simple_text_stream() {
        let mut state = ClaudeToOpenaiStreamState::new();

        // message_start
        let (_, d) = claude_event("message_start", json!({
            "type": "message_start",
            "message": {
                "id": "msg_1", "type": "message", "role": "assistant",
                "content": [], "model": "claude-3",
                "usage": {"input_tokens": 10, "output_tokens": 0}
            }
        }));
        let out0 = state.process_event("message_start", &d).unwrap();
        assert!(out0.is_empty()); // no output yet

        // content_block_start (text)
        let (_, d) = claude_event("content_block_start", json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }));
        let out1 = state.process_event("content_block_start", &d).unwrap();
        // Should emit first chunk with role=assistant
        let c1 = parse_openai_chunks(&out1);
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(c1[0]["choices"][0]["finish_reason"], json!(null));
        assert_eq!(c1[0]["model"], "claude-3");

        // content_block_delta (text_delta)
        let (_, d) = claude_event("content_block_delta", json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hello"}
        }));
        let out2 = state.process_event("content_block_delta", &d).unwrap();
        let c2 = parse_openai_chunks(&out2);
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0]["choices"][0]["delta"]["content"], "Hello");

        // content_block_stop
        let (_, d) = claude_event("content_block_stop", json!({"type":"content_block_stop","index":0}));
        let out3 = state.process_event("content_block_stop", &d).unwrap();
        assert!(out3.is_empty());

        // message_delta
        let (_, d) = claude_event("message_delta", json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 5}
        }));
        let out4 = state.process_event("message_delta", &d).unwrap();
        assert!(out4.is_empty()); // buffered

        // message_stop
        let (_, d) = claude_event("message_stop", json!({"type":"message_stop"}));
        let out5 = state.process_event("message_stop", &d).unwrap();
        let c5 = parse_openai_chunks(&out5);
        // Expect: finish_reason chunk, usage chunk, [DONE]
        assert_eq!(c5.len(), 3);
        assert_eq!(c5[0]["choices"][0]["delta"], json!({}));
        assert_eq!(c5[0]["choices"][0]["finish_reason"], "stop");
        assert_eq!(c5[1]["usage"]["prompt_tokens"], 10);
        assert_eq!(c5[1]["usage"]["completion_tokens"], 5);
        assert_eq!(c5[1]["usage"]["total_tokens"], 15);
        assert_eq!(c5[2], Value::String("[DONE]".to_string()));
    }

    #[test]
    fn test_c2o_with_tool_use() {
        let mut state = ClaudeToOpenaiStreamState::new();

        // message_start
        let (_, d) = claude_event("message_start", json!({
            "type": "message_start",
            "message": {"id":"msg_2","type":"message","role":"assistant","content":[],"model":"claude-3","usage":{"input_tokens":15,"output_tokens":0}}
        }));
        let _ = state.process_event("message_start", &d).unwrap();

        // content_block_start (tool_use)
        let (_, d) = claude_event("content_block_start", json!({
            "type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}
        }));
        let out1 = state.process_event("content_block_start", &d).unwrap();
        let c1 = parse_openai_chunks(&out1);
        // Expect: role chunk + tool_calls start chunk
        assert_eq!(c1.len(), 2);
        assert_eq!(c1[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(c1[1]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(c1[1]["choices"][0]["delta"]["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(c1[1]["choices"][0]["delta"]["tool_calls"][0]["type"], "function");
        assert_eq!(c1[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(c1[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "");

        // content_block_delta (input_json_delta)
        let (_, d) = claude_event("content_block_delta", json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"input_json_delta","partial_json":"{\"location\":\"SF\"}"}
        }));
        let out2 = state.process_event("content_block_delta", &d).unwrap();
        let c2 = parse_openai_chunks(&out2);
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(c2[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "{\"location\":\"SF\"}");

        // content_block_stop
        let (_, d) = claude_event("content_block_stop", json!({"type":"content_block_stop","index":0}));
        let out3 = state.process_event("content_block_stop", &d).unwrap();
        assert!(out3.is_empty());

        // message_delta
        let (_, d) = claude_event("message_delta", json!({
            "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
            "usage":{"output_tokens":20}
        }));
        let _ = state.process_event("message_delta", &d).unwrap();

        // message_stop
        let (_, d) = claude_event("message_stop", json!({"type":"message_stop"}));
        let out5 = state.process_event("message_stop", &d).unwrap();
        let c5 = parse_openai_chunks(&out5);
        assert_eq!(c5.len(), 3);
        assert_eq!(c5[0]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(c5[1]["usage"]["prompt_tokens"], 15);
        assert_eq!(c5[1]["usage"]["completion_tokens"], 20);
        assert_eq!(c5[1]["usage"]["total_tokens"], 35);
        assert_eq!(c5[2], Value::String("[DONE]".to_string()));
    }

    #[test]
    fn test_c2o_ping_ignored() {
        let mut state = ClaudeToOpenaiStreamState::new();
        let (_, d) = claude_event("ping", json!({"type":"ping"}));
        let out = state.process_event("ping", &d).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn test_c2o_usage_in_final_chunk() {
        let mut state = ClaudeToOpenaiStreamState::new();

        let (_, d) = claude_event("message_start", json!({
            "type":"message_start",
            "message":{"id":"msg_3","type":"message","role":"assistant","content":[],"model":"claude-3","usage":{"input_tokens":100,"output_tokens":0}}
        }));
        let _ = state.process_event("message_start", &d).unwrap();

        let (_, d) = claude_event("content_block_start", json!({
            "type":"content_block_start","index":0,"content_block":{"type":"text","text":""}
        }));
        let _ = state.process_event("content_block_start", &d).unwrap();

        let (_, d) = claude_event("content_block_delta", json!({
            "type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"response"}
        }));
        let _ = state.process_event("content_block_delta", &d).unwrap();

        let (_, d) = claude_event("content_block_stop", json!({"type":"content_block_stop","index":0}));
        let _ = state.process_event("content_block_stop", &d).unwrap();

        let (_, d) = claude_event("message_delta", json!({
            "type":"message_delta","delta":{"stop_reason":"max_tokens","stop_sequence":null},
            "usage":{"output_tokens":50}
        }));
        let _ = state.process_event("message_delta", &d).unwrap();

        let (_, d) = claude_event("message_stop", json!({"type":"message_stop"}));
        let out = state.process_event("message_stop", &d).unwrap();
        let chunks = parse_openai_chunks(&out);

        // finish_reason should be "length" (max_tokens → length)
        assert_eq!(chunks[0]["choices"][0]["finish_reason"], "length");
        // usage chunk
        assert_eq!(chunks[1]["usage"]["prompt_tokens"], 100);
        assert_eq!(chunks[1]["usage"]["completion_tokens"], 50);
        assert_eq!(chunks[1]["usage"]["total_tokens"], 150);
        // [DONE]
        assert_eq!(chunks[2], Value::String("[DONE]".to_string()));
    }

    #[test]
    fn test_c2o_text_delta_before_block_start() {
        // Some Claude streams send content_block_delta without a preceding content_block_start.
        // The state machine should still emit the role chunk on first delta.
        let mut state = ClaudeToOpenaiStreamState::new();

        let (_, d) = claude_event("message_start", json!({
            "type":"message_start",
            "message":{"id":"msg_x","type":"message","role":"assistant","content":[],"model":"claude-3","usage":{"input_tokens":5,"output_tokens":0}}
        }));
        let _ = state.process_event("message_start", &d).unwrap();

        // content_block_delta without content_block_start
        let (_, d) = claude_event("content_block_delta", json!({
            "type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}
        }));
        let out = state.process_event("content_block_delta", &d).unwrap();
        let chunks = parse_openai_chunks(&out);
        // Should emit role chunk + content chunk
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "Hi");
    }

    #[test]
    fn test_c2o_multiple_tool_calls() {
        let mut state = ClaudeToOpenaiStreamState::new();

        let (_, d) = claude_event("message_start", json!({
            "type":"message_start",
            "message":{"id":"msg_m","type":"message","role":"assistant","content":[],"model":"claude-3","usage":{"input_tokens":5,"output_tokens":0}}
        }));
        let _ = state.process_event("message_start", &d).unwrap();

        // First tool_use block (index 0)
        let (_, d) = claude_event("content_block_start", json!({
            "type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"toolu_a","name":"fn_a","input":{}}
        }));
        let out1 = state.process_event("content_block_start", &d).unwrap();
        let c1 = parse_openai_chunks(&out1);
        assert_eq!(c1[1]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);

        let (_, d) = claude_event("content_block_delta", json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"input_json_delta","partial_json":"{\"a\":1}"}
        }));
        let out2 = state.process_event("content_block_delta", &d).unwrap();
        let c2 = parse_openai_chunks(&out2);
        assert_eq!(c2[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);

        let (_, d) = claude_event("content_block_stop", json!({"type":"content_block_stop","index":0}));
        let _ = state.process_event("content_block_stop", &d).unwrap();

        // Second tool_use block (index 1)
        let (_, d) = claude_event("content_block_start", json!({
            "type":"content_block_start","index":1,
            "content_block":{"type":"tool_use","id":"toolu_b","name":"fn_b","input":{}}
        }));
        let out3 = state.process_event("content_block_start", &d).unwrap();
        let c3 = parse_openai_chunks(&out3);
        assert_eq!(c3[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 1);
        assert_eq!(c3[0]["choices"][0]["delta"]["tool_calls"][0]["id"], "toolu_b");

        let (_, d) = claude_event("content_block_delta", json!({
            "type":"content_block_delta","index":1,
            "delta":{"type":"input_json_delta","partial_json":"{\"b\":2}"}
        }));
        let out4 = state.process_event("content_block_delta", &d).unwrap();
        let c4 = parse_openai_chunks(&out4);
        assert_eq!(c4[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 1);

        let (_, d) = claude_event("content_block_stop", json!({"type":"content_block_stop","index":1}));
        let _ = state.process_event("content_block_stop", &d).unwrap();

        let (_, d) = claude_event("message_delta", json!({
            "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
            "usage":{"output_tokens":10}
        }));
        let _ = state.process_event("message_delta", &d).unwrap();

        let (_, d) = claude_event("message_stop", json!({"type":"message_stop"}));
        let out5 = state.process_event("message_stop", &d).unwrap();
        let c5 = parse_openai_chunks(&out5);
        assert_eq!(c5[0]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(c5[2], Value::String("[DONE]".to_string()));
    }
}

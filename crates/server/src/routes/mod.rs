//! Route handlers for the proxy's three public endpoints.
//!
//! - `POST /v1/chat/completions`  → OpenAI-format entry  (routes/openai.rs)
//! - `POST /v1/messages`          → Claude-format entry  (routes/claude.rs)
//! - `GET  /v1/models`            → list available models (routes/models.rs)
//!
//! The OpenAI and Claude handlers share the same pipeline (resolve model →
//! execute → return). The only difference is the `EntryFormat` they pass
//! to the executor, which controls cross-format translation. The shared
//! pipeline lives in this module as `proxy_request` + `stream_sse_response`.

pub mod claude;
pub mod models;
pub mod openai;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use chennix_adaptor::{Adaptor, ClaudeAdaptor, OpenaiAdaptor};
use chennix_common::{AuthContext, ChannelProvider, ProxyError, Usage};
use chennix_core::billing::BillingManager;
use chennix_core::executor::{actual_cost, EntryFormat, ExecutionContext, StreamBootstrap};
use chennix_core::tracker::{Tracker, UsageWriter};
use chennix_translator::stream_state::{ClaudeToOpenaiStreamState, OpenaiToClaudeStreamState};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::state::AppState;

/// Pick the adaptor instance for a given channel provider. Mirrors the
/// private `pick_adaptor` in the executor — both are stateless.
fn pick_adaptor(provider: ChannelProvider) -> Box<dyn Adaptor> {
    match provider {
        ChannelProvider::OpenaiCompatible => Box::new(OpenaiAdaptor::new()),
        ChannelProvider::Anthropic => Box::new(ClaudeAdaptor::new()),
    }
}



/// Lightweight SSE frame parser for Claude-format streams.
///
/// Claude sends events as `event: {type}\ndata: {json}\n\n`. The raw bytes
/// from `reqwest::bytes_stream()` may split across event boundaries, so we
/// buffer incomplete frames and emit complete `(event_type, data)` pairs.
struct SseFrameParser {
    buffer: String,
}

impl SseFrameParser {
    fn new() -> Self {
        Self {
            buffer: String::with_capacity(4096),
        }
    }

    /// Feed raw bytes from the upstream stream. Returns complete SSE frames
    /// as `(event_type, data_bytes)` pairs.
    fn feed(&mut self, chunk: &Bytes) -> Vec<(String, Bytes)> {
        let text = match std::str::from_utf8(chunk) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        self.buffer.push_str(text);

        let mut frames = Vec::new();

        // Split on double-newline (SSE event separator)
        while let Some(separator_pos) = self.buffer.find("\n\n") {
            let frame = self.buffer[..separator_pos].to_string();
            self.buffer.drain(..separator_pos + 2);

            let mut event_type = String::new();
            let mut data_lines = Vec::new();

            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    event_type = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.trim().to_string());
                }
            }

            if data_lines.is_empty() {
                continue;
            }

            let data_str = data_lines.join("\n");
            event_type = if event_type.is_empty() {
                "message".to_string()
            } else {
                event_type
            };

            frames.push((event_type, Bytes::from(data_str)));
        }

        frames
    }
}

/// Shared proxy pipeline used by both the OpenAI and Claude handlers.
///
/// 1. Extract model name from the body.
/// 2. Resolve via the normalizer (cache is loaded lazily on first access).
/// 3. Enforce token-level model limits.
/// 4. Build `ExecutionContext`.
/// 5. Branch on `stream: true`:
///    - non-stream: `executor.execute()` → return JSON.
///    - stream: `executor.execute_stream()` → return SSE stream with
///      per-chunk usage extraction + post-stream billing settlement.
pub async fn proxy_request(
    state: AppState,
    auth: AuthContext,
    entry_format: EntryFormat,
    body: serde_json::Value,
) -> Result<Response, ProxyError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let start = std::time::Instant::now();
    let (method, path) = match entry_format {
        EntryFormat::OpenAI => ("POST", "/v1/chat/completions"),
        EntryFormat::Claude => ("POST", "/v1/messages"),
    };
    let client_ip = auth.client_ip.clone();
    let user_id = auth.user.id;
    let token_id = auth.token.id;
    let client_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());

    // 1. Extract model name
    let model_name = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| ProxyError::InvalidRequest("missing 'model' field".into()))?;

    // 2. Resolve via normalizer (ensure cache is loaded first so the
    //    normalizer has its alias mapping).
    state.cache.get(state.storage.as_ref()).await?;
    let (model_id, canonical_name) = state
        .normalizer
        .resolve(model_name)
        .await?
        .ok_or_else(|| ProxyError::ModelNotFound(model_name.to_string()))?;

    // 3. Enforce token-level model limits
    if !auth.token.allows_model(&canonical_name) {
        let err = ProxyError::InvalidRequest(format!(
            "model '{}' is not allowed for this token",
            canonical_name
        ));
        log_request_entry(
            &state,
            &request_id,
            client_ip.as_deref(),
            method,
            path,
            client_model.as_deref(),
            Some(&canonical_name),
            None,
            None,
            None,
            None,
            err.http_status() as i64,
            start.elapsed().as_millis() as i64,
            false,
            Some(&err.to_string()),
            Some(user_id),
            Some(token_id),
            0,
        )
        .await;
        return Err(err);
    }

    // 4. Build ExecutionContext
    let ctx = ExecutionContext {
        user_id: auth.user.id,
        token_id: auth.token.id,
        user_group: auth.user.group.clone(),
        model_id,
        canonical_name: canonical_name.clone(),
    };

    // 5. Branch on stream
    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    if is_stream {
        match state
            .executor
            .execute_stream(
                &ctx,
                entry_format,
                body,
                state.storage.as_ref(),
                state.storage.as_ref(),
                state.storage.as_ref(),
            )
            .await
        {
            Ok(bootstrap) => Ok(stream_sse_response(
                bootstrap,
                state,
                model_id,
                request_id,
                start,
                client_model,
                canonical_name,
                client_ip,
                user_id,
                token_id,
            )),
            Err(e) => {
                // execute_stream failed (bootstrap phase) — per-key failures
                // are already tracked inside the executor. Record the
                // request-level audit row here.
                let err_str = e.to_string();
                log_request_entry(
                    &state,
                    &request_id,
                    client_ip.as_deref(),
                    method,
                    path,
                    client_model.as_deref(),
                    Some(&canonical_name),
                    None,
                    None,
                    None,
                    None,
                    e.http_status() as i64,
                    start.elapsed().as_millis() as i64,
                    true,
                    Some(&err_str),
                    Some(user_id),
                    Some(token_id),
                    0,
                )
                .await;
                Err(e)
            }
        }
    } else {
        match state
            .executor
            .execute(
                &ctx,
                entry_format,
                body,
                state.storage.as_ref(),
                state.storage.as_ref(),
                state.storage.as_ref(),
            )
            .await
        {
            Ok(result) => {
                // 审计日志：非流式成功。executor 已通过 track_success 写入
                // usage_logs；此处补全 request_logs 的渠道/key/cost 等审计
                // 字段（之前因 execute 只返回 Bytes 而丢失这些信息）。
                let attempted_keys_csv = if result.attempted_keys.is_empty() {
                    None
                } else {
                    Some(result.attempted_keys.join(","))
                };
                log_request_entry(
                    &state,
                    &request_id,
                    client_ip.as_deref(),
                    method,
                    path,
                    client_model.as_deref(),
                    Some(&canonical_name),
                    Some(&result.channel_name),
                    result.key_label.as_deref(),
                    attempted_keys_csv.as_deref(),
                    result.upstream_status,
                    200,
                    start.elapsed().as_millis() as i64,
                    false,
                    None,
                    Some(user_id),
                    Some(token_id),
                    result.quota_cost,
                )
                .await;
                Ok(Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(Body::from(result.body))
                    .unwrap())
            }
            Err(e) => {
                // execute failed — per-key failures are already tracked
                // inside the executor. Record the request-level audit row.
                let err_str = e.to_string();
                log_request_entry(
                    &state,
                    &request_id,
                    client_ip.as_deref(),
                    method,
                    path,
                    client_model.as_deref(),
                    Some(&canonical_name),
                    None,
                    None,
                    None,
                    None,
                    e.http_status() as i64,
                    start.elapsed().as_millis() as i64,
                    false,
                    Some(&err_str),
                    Some(user_id),
                    Some(token_id),
                    0,
                )
                .await;
                Err(e)
            }
        }
    }
}

/// Best-effort request_logs insertion. Errors are logged but never
/// propagated — request logging must not affect the response path.
async fn log_request_entry(
    state: &AppState,
    request_id: &str,
    client_ip: Option<&str>,
    method: &str,
    path: &str,
    client_model: Option<&str>,
    normalized_model: Option<&str>,
    channel_name: Option<&str>,
    key_label: Option<&str>,
    attempted_keys: Option<&str>,
    upstream_status: Option<i64>,
    response_status: i64,
    duration_ms: i64,
    stream: bool,
    error_message: Option<&str>,
    user_id: Option<i64>,
    token_id: Option<i64>,
    quota_cost: i64,
) {
    if let Err(e) = state
        .storage
        .log_request(
            request_id,
            client_ip,
            method,
            path,
            client_model,
            normalized_model,
            channel_name,
            key_label,
            attempted_keys,
            upstream_status,
            response_status,
            duration_ms,
            stream,
            error_message,
            user_id,
            token_id,
            quota_cost,
        )
        .await
    {
        tracing::error!("log_request failed: {}", e);
    }
}

/// RAII guard that decrements the active streaming task counter when
/// dropped. Ensures the counter is always decremented, even if the
/// spawned task panics.
struct ActiveStreamGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Build an SSE streaming response from a `StreamBootstrap`.
///
/// Spawns a background task that:
/// 1. Reads chunks from the upstream `reqwest::Response`.
/// 2. Calls `adaptor.extract_usage()` per chunk to accumulate usage.
/// 3. If cross-format: translates chunks via the appropriate stream state machine.
/// 4. Forwards each chunk to the client via an mpsc channel.
/// 5. On stream end: settles billing with the accumulated usage and
///    tracks the usage row, then writes a `request_logs` audit row.
///
/// If the client disconnects mid-stream (channel send fails), the task
/// stops forwarding but still attempts billing settlement + tracking
/// with the usage accumulated so far.
///
/// Additional params beyond `bootstrap`/`state`/`model_id` carry the
/// audit context (request id, client ip, client/normalized model,
/// timing) needed for the `request_logs` row. They are captured into
/// the spawned task.
#[allow(clippy::too_many_arguments)]
fn stream_sse_response(
    bootstrap: StreamBootstrap,
    state: AppState,
    model_id: i64,
    request_id: String,
    start: std::time::Instant,
    client_model: Option<String>,
    canonical_name: String,
    client_ip: Option<String>,
    user_id: i64,
    token_id: i64,
) -> Response {
    let StreamBootstrap {
        response,
        mut session,
        routed_key,
        model_pricing,
        entry_format,
        adaptor_provider,
    } = bootstrap;

    let adaptor = pick_adaptor(routed_key.channel.provider);
    let channel_id = routed_key.channel.id;
    let key_id = routed_key.key.id;
    let channel_name = routed_key.channel.name.clone();
    let key_label = routed_key.key.label.clone();
    let attempted_keys = routed_key.key.label.clone();
    let upstream_model_name = routed_key.upstream_model_name.clone();

    let needs_translation = entry_format.provider() != adaptor_provider;

    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    let storage = state.storage.clone();
    let health = state.health.clone();
    let cache = state.cache.clone();
    let state_for_log = state.storage.clone();

    // Track this streaming task for graceful shutdown. The guard
    // decrements the counter when the task completes (or panics).
    let active_streams = state.active_streams.clone();
    active_streams.fetch_add(1, Ordering::SeqCst);

    tokio::spawn(async move {
        let _guard = ActiveStreamGuard { counter: active_streams };
        let mut upstream = response.bytes_stream();
        let mut usage_acc = Usage::default();

        // Cross-format translation state machines (created on demand).
        let mut o2c_state = if needs_translation
            && entry_format == EntryFormat::OpenAI
            && adaptor_provider == ChannelProvider::Anthropic
        {
            Some(OpenaiToClaudeStreamState::new())
        } else {
            None
        };
        let mut c2o_state = if needs_translation
            && entry_format == EntryFormat::Claude
            && adaptor_provider == ChannelProvider::OpenaiCompatible
        {
            Some(ClaudeToOpenaiStreamState::new())
        } else {
            None
        };
        let mut claude_parser = if c2o_state.is_some() {
            Some(SseFrameParser::new())
        } else {
            None
        };

        while let Some(chunk_result) = upstream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    // Always extract usage from the raw upstream chunk
                    // (before translation) for billing purposes.
                    if let Some(u) = adaptor.extract_usage(&chunk) {
                        usage_acc.add(&u);
                    }

                    if let Some(ref mut o2c) = o2c_state {
                        // OpenAI upstream → Claude client
                        match o2c.process_chunk(&chunk) {
                            Ok(events) => {
                                for event in events {
                                    if tx.send(Ok(event)).await.is_err() {
                                        tracing::warn!("client disconnected from stream");
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("O2C stream translation error: {}", e);
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        e.to_string(),
                                    )))
                                    .await;
                                break;
                            }
                        }
                    } else if let Some(ref mut c2o) = c2o_state {
                        // Claude upstream → OpenAI client
                        let parser = claude_parser.as_mut().unwrap();
                        let frames = parser.feed(&chunk);
                        for (event_type, data) in frames {
                            match c2o.process_event(&event_type, &data) {
                                Ok(chunks) => {
                                    for c in chunks {
                                        if tx.send(Ok(c)).await.is_err() {
                                            tracing::warn!("client disconnected from stream");
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("C2O stream translation error: {}", e);
                                    let _ = tx
                                        .send(Err(std::io::Error::new(
                                            std::io::ErrorKind::InvalidData,
                                            e.to_string(),
                                        )))
                                        .await;
                                    break;
                                }
                            }
                        }
                    } else {
                        // Same-format passthrough
                        if tx.send(Ok(chunk)).await.is_err() {
                            tracing::warn!("client disconnected from stream");
                            break;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        )))
                        .await;
                    break;
                }
            }
        }

        // stream ended (or client disconnected) — settle billing
        let cost = actual_cost(&usage_acc, model_pricing.as_ref());
        if let Err(e) = BillingManager::settle(storage.as_ref(), &mut session, cost).await {
            tracing::error!("streaming billing settle failed: {}", e);
        }

        // track usage
        if let Err(e) = Tracker::track_success(
            storage.as_ref(),
            health.as_ref(),
            cache.as_ref(),
            user_id,
            token_id,
            channel_id,
            key_id,
            model_id,
            &upstream_model_name,
            &usage_acc,
            cost,
            "chat",
        )
        .await
        {
            tracing::error!("streaming track_success failed: {}", e);
        }

        // 审计日志：流式请求结束（成功或客户端断开）。best-effort，不阻塞响应。
        if let Err(e) = state_for_log
            .log_request(
                &request_id,
                client_ip.as_deref(),
                "POST",
                "", // path 已在 proxy_request 中按 entry_format 决定，此处留空（流式记录）
                client_model.as_deref(),
                Some(canonical_name.as_str()),
                Some(channel_name.as_str()),
                key_label.as_deref(),
                attempted_keys.as_deref(),
                None,
                200,
                start.elapsed().as_millis() as i64,
                true,
                None,
                Some(user_id),
                Some(token_id),
                cost,
            )
            .await
        {
            tracing::error!("streaming log_request failed: {}", e);
        }
    });

    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_frame_parser_simple() {
        let mut parser = SseFrameParser::new();
        let chunk = Bytes::from("event: message_start\ndata: {\"type\":\"message_start\"}\n\n");
        let frames = parser.feed(&chunk);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, "message_start");
        let data: serde_json::Value = serde_json::from_slice(&frames[0].1).unwrap();
        assert_eq!(data["type"], "message_start");
    }

    #[test]
    fn test_sse_frame_parser_multiple_events() {
        let mut parser = SseFrameParser::new();
        let chunk = Bytes::from(
            "event: content_block_start\ndata: {\"type\":\"content_block_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n",
        );
        let frames = parser.feed(&chunk);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0, "content_block_start");
        assert_eq!(frames[1].0, "content_block_delta");
    }

    #[test]
    fn test_sse_frame_parser_split_across_chunks() {
        let mut parser = SseFrameParser::new();
        // First chunk: partial event
        let chunk1 = Bytes::from("event: message_start\ndata: {\"type\":\"");
        let frames1 = parser.feed(&chunk1);
        assert!(frames1.is_empty());
        // Second chunk: rest of the event
        let chunk2 = Bytes::from("message_start\"}\n\n");
        let frames2 = parser.feed(&chunk2);
        assert_eq!(frames2.len(), 1);
        assert_eq!(frames2[0].0, "message_start");
    }

    #[test]
    fn test_sse_frame_parser_no_event_type() {
        let mut parser = SseFrameParser::new();
        let chunk = Bytes::from("data: {\"type\":\"ping\"}\n\n");
        let frames = parser.feed(&chunk);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, "message"); // default event type
    }

    #[test]
    fn test_sse_frame_parser_multiline_data() {
        let mut parser = SseFrameParser::new();
        let chunk = Bytes::from("event: message_delta\ndata: {\"type\":\"message_delta\"\ndata: ,\"usage\":{\"output_tokens\":5}}\n\n");
        let frames = parser.feed(&chunk);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, "message_delta");
        // Multi-line data should be joined with \n
        let data_str = std::str::from_utf8(&frames[0].1).unwrap();
        assert!(data_str.contains("message_delta"));
    }
}

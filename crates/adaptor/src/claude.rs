use async_trait::async_trait;
use bytes::Bytes;
use chennix_common::{ChannelProvider, ProxyError, ProxyResult, Usage};
use std::collections::HashMap;
use crate::traits::Adaptor;

pub struct ClaudeAdaptor;

impl ClaudeAdaptor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Adaptor for ClaudeAdaptor {
    fn provider(&self) -> ChannelProvider {
        ChannelProvider::Anthropic
    }

    async fn execute(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        body: serde_json::Value,
        mut headers: HashMap<String, String>,
    ) -> ProxyResult<(u16, Bytes)> {
        headers.insert("x-api-key".into(), api_key.into());
        headers.insert("anthropic-version".into(), "2023-06-01".into());
        headers.insert("Content-Type".into(), "application/json".into());

        let url = build_claude_messages_url(base_url);
        let resp = client
            .post(&url)
            .headers(headers_to_headermap(&headers)?)
            .json(&body)
            .send()
            .await
            .map_err(ProxyError::Http)?;

        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(ProxyError::Http)?;

        if status >= 400 {
            let body_str = String::from_utf8_lossy(&bytes).to_string();
            return Err(ProxyError::Upstream { status, body: body_str });
        }
        Ok((status, bytes))
    }

    async fn execute_stream(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        body: serde_json::Value,
        mut headers: HashMap<String, String>,
    ) -> ProxyResult<reqwest::Response> {
        headers.insert("x-api-key".into(), api_key.into());
        headers.insert("anthropic-version".into(), "2023-06-01".into());
        headers.insert("Content-Type".into(), "application/json".into());

        let url = build_claude_messages_url(base_url);
        let resp = client
            .post(&url)
            .headers(headers_to_headermap(&headers)?)
            .json(&body)
            .send()
            .await
            .map_err(ProxyError::Http)?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let bytes = resp.bytes().await.map_err(ProxyError::Http)?;
            let body_str = String::from_utf8_lossy(&bytes).to_string();
            return Err(ProxyError::Upstream { status, body: body_str });
        }
        Ok(resp)
    }

    fn extract_usage(&self, chunk: &Bytes) -> Option<Usage> {
        // Claude 流式: event: message_delta\ndata: {json}\n\n
        // usage 分布在两类事件中：
        // - message_start → message.usage.input_tokens（请求的 prompt token 数）
        // - message_delta → usage.output_tokens（累积的 completion token 数）
        //
        // 一个 chunk 可能包含多个 data: 行（message_start + message_delta
        // 可能在同一个 TCP 包内），因此不能在第一个匹配时 return，
        // 必须遍历所有行并合并 input/output。
        let text = String::from_utf8_lossy(chunk);
        let mut input_tokens: Option<u64> = None;
        let mut output_tokens: Option<u64> = None;
        for line in text.lines() {
            if let Some(json_str) = line.strip_prefix("data: ") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                    // message_delta: 提取 output_tokens（累积值）
                    if v.get("type").and_then(|t| t.as_str()) == Some("message_delta") {
                        if let Some(usage) = v.get("usage") {
                            output_tokens =
                                usage.get("output_tokens").and_then(|t| t.as_u64());
                        }
                    }
                    // message_start: 提取 input_tokens
                    if v.get("type").and_then(|t| t.as_str()) == Some("message_start") {
                        if let Some(msg) = v.get("message") {
                            if let Some(usage) = msg.get("usage") {
                                input_tokens =
                                    usage.get("input_tokens").and_then(|t| t.as_u64());
                            }
                        }
                    }
                }
            }
        }
        // 至少找到一个才返回
        if input_tokens.is_some() || output_tokens.is_some() {
            let input = input_tokens.unwrap_or(0);
            let output = output_tokens.unwrap_or(0);
            Some(Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
            })
        } else {
            None
        }
    }
}

fn headers_to_headermap(headers: &HashMap<String, String>) -> ProxyResult<reqwest::header::HeaderMap> {
    let mut map = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| ProxyError::Config(e.to_string()))?;
        let val = reqwest::header::HeaderValue::from_str(v)
            .map_err(|e| ProxyError::Config(e.to_string()))?;
        map.insert(name, val);
    }
    Ok(map)
}

/// 构造 Claude Messages 端点 URL。
///
/// 兼容两种用户输入：
/// - `https://api.anthropic.com`        → `https://api.anthropic.com/v1/messages`
/// - `https://api.anthropic.com/v1`     → `https://api.anthropic.com/v1/messages`
///
/// 判断依据：base_url 末尾路径段是否为版本号（如 `v1`）。
/// 若是，直接拼接 `/messages`；否则补 `/v1/messages`。
pub fn build_claude_messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or("");
    let is_versioned = last_segment.starts_with('v')
        && last_segment[1..].chars().next().map_or(false, |c| c.is_ascii_digit());
    if is_versioned {
        format!("{}/messages", trimmed)
    } else {
        format!("{}/v1/messages", trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_usage_message_delta() {
        let adaptor = ClaudeAdaptor::new();
        let chunk = Bytes::from("event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n");
        let usage = adaptor.extract_usage(&chunk).unwrap();
        assert_eq!(usage.completion_tokens, 42);
    }

    #[test]
    fn test_extract_usage_message_start() {
        let adaptor = ClaudeAdaptor::new();
        let chunk = Bytes::from("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100}}}\n\n");
        let usage = adaptor.extract_usage(&chunk).unwrap();
        assert_eq!(usage.prompt_tokens, 100);
    }

    #[test]
    fn test_build_url_without_version() {
        assert_eq!(
            build_claude_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_build_url_with_v1() {
        // 不应出现 /v1/v1/messages
        assert_eq!(
            build_claude_messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_build_url_with_trailing_slash() {
        assert_eq!(
            build_claude_messages_url("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com/v1/messages"
        );
    }
}

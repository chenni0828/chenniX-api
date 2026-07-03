use async_trait::async_trait;
use bytes::Bytes;
use chennix_common::{ChannelProvider, ProxyError, ProxyResult, Usage};
use std::collections::HashMap;
use crate::traits::Adaptor;

pub struct OpenaiAdaptor;

impl OpenaiAdaptor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Adaptor for OpenaiAdaptor {
    fn provider(&self) -> ChannelProvider {
        ChannelProvider::OpenaiCompatible
    }

    async fn execute(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        body: serde_json::Value,
        mut headers: HashMap<String, String>,
    ) -> ProxyResult<(u16, Bytes)> {
        headers.insert("Authorization".into(), format!("Bearer {}", api_key));
        headers.insert("Content-Type".into(), "application/json".into());

        let url = build_openai_chat_url(base_url);
        let resp = client
            .post(&url)
            .headers(headers_to_headermap(&headers)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProxyError::Http(e))?;

        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| ProxyError::Http(e))?;

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
        mut body: serde_json::Value,
        mut headers: HashMap<String, String>,
    ) -> ProxyResult<reqwest::Response> {
        headers.insert("Authorization".into(), format!("Bearer {}", api_key));
        headers.insert("Content-Type".into(), "application/json".into());

        // 主动注入 stream_options.include_usage = true
        if let Some(stream) = body.get("stream").and_then(|v| v.as_bool()) {
            if stream {
                body["stream_options"] = serde_json::json!({ "include_usage": true });
            }
        }

        let url = build_openai_chat_url(base_url);
        let resp = client
            .post(&url)
            .headers(headers_to_headermap(&headers)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProxyError::Http(e))?;

        let status = resp.status().as_u16();
        if status >= 400 {
            let bytes = resp.bytes().await.map_err(|e| ProxyError::Http(e))?;
            let body_str = String::from_utf8_lossy(&bytes).to_string();
            return Err(ProxyError::Upstream { status, body: body_str });
        }
        Ok(resp)
    }

    fn extract_usage(&self, chunk: &Bytes) -> Option<Usage> {
        // OpenAI 流式: data: {json}\n\n, 最后一个 chunk 里有 usage。
        // 一个 chunk 可能包含多个 data: 行，usage 是累积值，
        // 取最后一个带 usage 的行（而非第一个 return）。
        let text = String::from_utf8_lossy(chunk);
        let mut found: Option<Usage> = None;
        for line in text.lines() {
            if let Some(json_str) = line.strip_prefix("data: ") {
                if json_str.trim() == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                    if let Some(usage) = v.get("usage") {
                        if !usage.is_null() {
                            found = Some(Usage {
                                prompt_tokens: usage.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
                                completion_tokens: usage.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
                                total_tokens: usage.get("total_tokens").and_then(|t| t.as_u64()).unwrap_or(0),
                            });
                        }
                    }
                }
            }
        }
        found
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

/// 构造 OpenAI Chat Completions 端点 URL。
///
/// 兼容两种用户输入：
/// - `https://api.openai.com`        → `https://api.openai.com/v1/chat/completions`
/// - `https://api.openai.com/v1`     → `https://api.openai.com/v1/chat/completions`
///
/// 判断依据：base_url 末尾路径段是否为版本号（如 `v1`、`v2`、`v1beta`）。
/// 若是，直接拼接 `/chat/completions`；否则补 `/v1/chat/completions`。
pub fn build_openai_chat_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    // 检查末尾是否为版本号段（v + 数字，或 v + 数字 + 可选字母后缀如 v1beta）
    let last_segment = trimmed.rsplit('/').next().unwrap_or("");
    let is_versioned = last_segment.starts_with('v')
        && last_segment[1..].chars().next().map_or(false, |c| c.is_ascii_digit());
    if is_versioned {
        format!("{}/chat/completions", trimmed)
    } else {
        format!("{}/v1/chat/completions", trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_usage_from_chunk() {
        let adaptor = OpenaiAdaptor::new();
        let chunk = Bytes::from("data: {\"id\":\"1\",\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\ndata: [DONE]\n\n");
        let usage = adaptor.extract_usage(&chunk).unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_extract_usage_none() {
        let adaptor = OpenaiAdaptor::new();
        let chunk = Bytes::from("data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n");
        assert!(adaptor.extract_usage(&chunk).is_none());
    }

    #[test]
    fn test_build_url_without_version() {
        // 用户填裸域名，应自动补 /v1
        assert_eq!(
            build_openai_chat_url("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_url_with_v1() {
        // 用户已填 /v1，不应重复添加
        assert_eq!(
            build_openai_chat_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_url_with_trailing_slash() {
        // 末尾斜杠应被正确处理
        assert_eq!(
            build_openai_chat_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_build_url_with_v1beta() {
        // 版本号后缀带字母也应识别
        assert_eq!(
            build_openai_chat_url("https://generativelanguage.googleapis.com/v1beta"),
            "https://generativelanguage.googleapis.com/v1beta/chat/completions"
        );
    }

    #[test]
    fn test_build_url_with_path_segment() {
        // 末尾非版本号段（如 azure deployment）不应被识别为版本
        assert_eq!(
            build_openai_chat_url("https://xxx.openai.azure.com/openai/deployments/gpt-4o"),
            "https://xxx.openai.azure.com/openai/deployments/gpt-4o/v1/chat/completions"
        );
    }
}

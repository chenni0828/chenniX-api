pub mod claude;
pub mod openai;
pub mod traits;

pub use claude::ClaudeAdaptor;
pub use openai::OpenaiAdaptor;
pub use traits::Adaptor;

/// 公开的 URL 构造工具，供 admin handlers 与 adaptor 共用。
/// 避免在多处重复相同的拼接逻辑导致不一致。
pub use claude::build_claude_messages_url;
pub use openai::build_openai_chat_url;

/// 构造 OpenAI / Anthropic 风格的 `/models` 列表端点 URL。
///
/// - 末尾是版本号段（如 `v1`、`v1beta`）→ 直接拼 `/models`
/// - 否则 → 补 `/v1/models`
pub fn build_models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let last_segment = trimmed.rsplit('/').next().unwrap_or("");
    let is_versioned = last_segment.starts_with('v')
        && last_segment[1..]
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_digit());
    if is_versioned {
        format!("{}/models", trimmed)
    } else {
        format!("{}/v1/models", trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_models_url_without_version() {
        assert_eq!(
            build_models_url("https://api.openai.com"),
            "https://api.openai.com/v1/models"
        );
    }

    #[test]
    fn test_build_models_url_with_v1() {
        assert_eq!(
            build_models_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/models"
        );
    }
}

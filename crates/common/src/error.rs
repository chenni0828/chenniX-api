use thiserror::Error;
use chrono::{DateTime, Utc};

pub type ProxyResult<T> = Result<T, ProxyError>;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("client auth failed")]
    ClientAuthFailed,

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("all keys disabled for model: {model}")]
    AllKeysDisabled { model: String },

    #[error("all keys in cooldown for model: {model}, earliest recovery: {earliest_recovery:?}")]
    AllKeysCooldown { model: String, earliest_recovery: Option<DateTime<Utc>> },

    #[error("all keys quota exhausted for model: {model}")]
    AllKeysQuotaExhausted { model: String },

    #[error("all keys exhausted for model: {model}, attempted: {attempted_keys:?}, last: {last_error:?}")]
    AllKeysExhausted { model: String, attempted_keys: Vec<String>, last_error: Option<String> },

    #[error("upstream error: status {status}, body: {body}")]
    Upstream { status: u16, body: String },

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("translator error: {0}")]
    Translator(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("upstream timeout after {0:?}")]
    UpstreamTimeout(std::time::Duration),
}

impl ProxyError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Upstream { status, .. } => *status == 429 || (*status >= 500 && *status < 600),
            // 上游超时是临时性故障，应允许重试下一个 key
            Self::UpstreamTimeout(_) => true,
            _ => false,
        }
    }
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::Upstream { status, .. } => *status == 401 || *status == 403,
            _ => false,
        }
    }
    pub fn is_invalid_request(&self) -> bool {
        match self {
            Self::Upstream { status, .. } => *status == 400 || *status == 422,
            Self::InvalidRequest(_) => true,
            _ => false,
        }
    }
    pub fn http_status(&self) -> u16 {
        match self {
            Self::ClientAuthFailed => 401,
            Self::ModelNotFound(_) => 404,
            Self::AllKeysDisabled { .. } | Self::AllKeysCooldown { .. }
            | Self::AllKeysQuotaExhausted { .. } | Self::AllKeysExhausted { .. } => 503,
            Self::Upstream { status, .. } => *status,
            Self::InvalidRequest(_) => 400,
            Self::Translator(_) => 502,
            Self::Storage(_) | Self::Config(_) => 500,
            Self::Io(_) | Self::Json(_) | Self::Http(_) => 500,
            // 网关超时
            Self::UpstreamTimeout(_) => 504,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_retryable() {
        assert!(ProxyError::Upstream { status: 429, body: "".into() }.is_retryable());
        assert!(ProxyError::Upstream { status: 503, body: "".into() }.is_retryable());
        assert!(!ProxyError::Upstream { status: 400, body: "".into() }.is_retryable());
    }
    #[test]
    fn test_fatal() {
        assert!(ProxyError::Upstream { status: 401, body: "".into() }.is_fatal());
        assert!(ProxyError::Upstream { status: 403, body: "".into() }.is_fatal());
    }
    #[test]
    fn test_invalid_request() {
        assert!(ProxyError::Upstream { status: 400, body: "".into() }.is_invalid_request());
        assert!(ProxyError::InvalidRequest("bad".into()).is_invalid_request());
        assert!(!ProxyError::Upstream { status: 429, body: "".into() }.is_invalid_request());
    }
}

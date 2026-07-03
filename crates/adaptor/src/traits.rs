use chennix_common::{ChannelProvider, ProxyResult, Usage};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;

/// 上游适配器 trait, 每个 provider 一个实现
/// Adaptor 内部决定同格式 (完整反序列化+适配) 还是跨格式 (走 Translator)
#[async_trait]
pub trait Adaptor: Send + Sync {
    fn provider(&self) -> ChannelProvider;

    /// 非流式请求: 发送到上游, 返回响应 body
    ///
    /// 使用调用方传入的共享 `client`（带连接池复用），避免每次请求都新建 Client。
    async fn execute(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        body: serde_json::Value,
        headers: HashMap<String, String>,
    ) -> ProxyResult<(u16, Bytes)>;

    /// 流式请求: 发送到上游, 返回 SSE stream (每个 item 是一个 chunk: Bytes)
    ///
    /// 使用调用方传入的共享 `client`（带连接池复用），避免每次请求都新建 Client。
    async fn execute_stream(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
        body: serde_json::Value,
        headers: HashMap<String, String>,
    ) -> ProxyResult<reqwest::Response>;

    /// 从流式 chunk 提取 usage (每个 provider 的 usage 位置不同)
    fn extract_usage(&self, chunk: &Bytes) -> Option<Usage>;
}

//! Request executor: ties routing, health, billing, tracking, adaptor and
//! translator together into the proxy's per-request pipeline.
//!
//! ## Non-streaming (`execute`)
//! 1. Resolve cached channels + keys for the model.
//! 2. `Router::route` → flat sorted candidate list.
//! 3. `BillingManager::pre_charge` (estimate).
//! 4. For each candidate key (in order):
//!    a. Skip if `HealthManager::is_available` is false.
//!    b. Build the outgoing body (same-format → swap `model`; cross-format →
//!       translate via `chennix-translator`, then swap `model`).
//!    c. Call the appropriate `Adaptor`.
//!    d. On success: extract usage, settle billing, track success, return.
//!    e. On retryable upstream error (429/5xx/network): mark cooldown, next.
//!    f. On fatal upstream error (401/403): mark disabled, next.
//!    g. On invalid_request (400/422): refund + return immediately.
//! 5. If all keys exhausted: refund + return `AllKeysExhausted`.
//!
//! ## Streaming (`execute_stream`)
//! Same as above through step (c), but the per-key "call adaptor" step
//! returns a `reqwest::Response` once the upstream has accepted the request
//! and started streaming. The bootstrap boundary is the adaptor returning
//! `Ok(resp)` — at that point we are committed and cannot retry. Chunk
//! forwarding + per-chunk usage extraction happens in the HTTP handler
//! (Task 25/26), not here.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use chennix_common::{BillingType, ChannelModelPricing, ChannelProvider, ProxyError, ProxyResult, Usage};

use chennix_adaptor::{Adaptor, ClaudeAdaptor, OpenaiAdaptor};

use crate::billing::{BillingManager, BillingRepo};
use crate::billing_expr;
use crate::cache::{ConfigCache, RoutingStrategy};
use crate::health::HealthManager;
use crate::router::{RoutedKey, Router};
use crate::tracker::{Tracker, UsageWriter};

/// The wire format the *client* used to talk to us. Determines whether the
/// request needs cross-format translation before being sent upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryFormat {
    OpenAI,
    Claude,
}

impl EntryFormat {
    pub fn provider(&self) -> ChannelProvider {
        match self {
            Self::OpenAI => ChannelProvider::OpenaiCompatible,
            Self::Claude => ChannelProvider::Anthropic,
        }
    }
}

/// Per-request context passed by the auth middleware.
pub struct ExecutionContext {
    pub user_id: i64,
    pub token_id: i64,
    pub user_group: String,
    pub model_id: i64,
    pub canonical_name: String,
}

pub struct Executor {
    pub health: Arc<HealthManager>,
    pub cache: Arc<ConfigCache>,
    /// 共享的 HTTP Client（连接池复用），所有上游请求都通过它发送。
    pub http_client: reqwest::Client,
    /// 非流式上游请求整体超时。
    pub upstream_timeout: std::time::Duration,
    /// 流式请求首字节到达超时（不中断已建立的流）。
    pub streaming_timeout: std::time::Duration,
}

/// What the executor should do after a key attempt fails.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FailureAction {
    /// Cooldown the key and try the next candidate.
    Cooldown,
    /// Disable the key (e.g. 401/403) and try the next candidate.
    Disable,
    /// Refund billing and surface the error to the client immediately.
    ReturnToClient,
}

/// Classify an upstream error into the executor's next action.
///
/// - `is_invalid_request` (400/422 / `InvalidRequest`) → `ReturnToClient`
///   (the request itself is bad; no point trying other keys).
/// - `is_fatal` (401/403) → `Disable` (key is bad; don't retry it).
/// - `is_retryable` (429/5xx) → `Cooldown` (transient; try next).
/// - Anything else (e.g. `Http` network error, `Translator`) → `Cooldown`
///   (be optimistic; try the next key).
fn classify_failure(e: &ProxyError) -> FailureAction {
    if e.is_invalid_request() {
        FailureAction::ReturnToClient
    } else if e.is_fatal() {
        FailureAction::Disable
    } else {
        // retryable Upstream, Http network errors, Translator errors,
        // unknown — all treated as "try the next key".
        FailureAction::Cooldown
    }
}

/// Estimate the pre-charge cost for a single request.
///
/// Pricing is determined by the per-binding `ChannelModelPricing`
/// (model_id + channel_id). When the pricing is not configured
/// (`is_configured()` is false), the request is free (cost = 0).
///
/// Quota unit = yuan (元). Token prices are in 元/1K tokens; per-call
/// price is in 元/次; expression result is in 元.
///
/// `max_tokens` — the client-declared completion cap (from the request
/// body's `max_tokens` / `max_completion_tokens` field). When `None`,
/// defaults to 500 (the same floor new-api uses). When `Some(n)`, the
/// estimated completion is `n` — this aligns with new-api's
/// `preConsumedTokens = max(promptTokens, PreConsumedQuota) + meta.MaxTokens`
/// (see `relay/helper/price.go:89-92`). The prompt floor is 500
/// (`PreConsumedQuota`), matching new-api's default `CountToken=false`
/// path which does not estimate prompt tokens from the body.
fn estimate_cost(pricing: Option<&ChannelModelPricing>, max_tokens: Option<u64>) -> i64 {
    const ASSUMED_PROMPT: f64 = 500.0;
    let assumed_completion = max_tokens.unwrap_or(500) as f64;
    let Some(p) = pricing.filter(|p| p.is_configured()) else {
        return 0;
    };
    match p.billing_type {
        BillingType::Token => {
            let cost = (ASSUMED_PROMPT / 1000.0) * p.input_price
                + (assumed_completion / 1000.0) * p.output_price;
            (cost.round() as i64).max(1)
        }
        BillingType::PerCall => (p.call_price.round() as i64).max(1),
        BillingType::Expression => {
            let expr = p
                .billing_expr
                .as_deref()
                .unwrap_or("0");
            match billing_expr::eval(expr, ASSUMED_PROMPT as u64, assumed_completion as u64) {
                Ok(v) => (v.round() as i64).max(1),
                Err(_) => 0,
            }
        }
    }
}

/// Extract the client-declared completion cap (`max_tokens` or
/// `max_completion_tokens`) from the request body.
///
/// OpenAI format: reads `max_tokens` or `max_completion_tokens` (whichever
/// is set; the latter is the newer field name).
/// Claude format: reads `max_tokens` (required field in Claude API).
///
/// Returns `None` when the field is absent or not a positive integer.
/// 对齐 new-api `fastTokenCountMetaForPricing`（controller/relay.go:264-291）
/// 的 max_tokens 提取逻辑。
fn extract_max_tokens(body: &serde_json::Value, entry_format: EntryFormat) -> Option<u64> {
    let val = match entry_format {
        EntryFormat::OpenAI => body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens")),
        EntryFormat::Claude => body.get("max_tokens"),
    }?;
    let n = val.as_u64()?;
    (n > 0).then_some(n)
}

/// Compute the actual cost from observed usage.
///
/// Pricing is determined by the per-binding `ChannelModelPricing`.
/// When not configured, cost = 0 (free).
pub fn actual_cost(
    usage: &Usage,
    pricing: Option<&ChannelModelPricing>,
) -> i64 {
    let Some(p) = pricing.filter(|p| p.is_configured()) else {
        return 0;
    };
    match p.billing_type {
        BillingType::Token => {
            let input_cost = (usage.prompt_tokens as f64 / 1000.0) * p.input_price;
            let output_cost = (usage.completion_tokens as f64 / 1000.0) * p.output_price;
            (input_cost + output_cost).round() as i64
        }
        BillingType::PerCall => p.call_price.round() as i64,
        BillingType::Expression => {
            let expr = p
                .billing_expr
                .as_deref()
                .unwrap_or("0");
            match billing_expr::eval(expr, usage.prompt_tokens, usage.completion_tokens) {
                Ok(v) => v.round() as i64,
                Err(_) => 0,
            }
        }
    }
}

/// Pull the upstream native format usage out of a non-streaming response body.
fn extract_usage_from_response(body: &serde_json::Value, adaptor_provider: ChannelProvider) -> Usage {
    match adaptor_provider {
        ChannelProvider::OpenaiCompatible => {
            let u = body.get("usage");
            Usage {
                prompt_tokens: u
                    .and_then(|u| u.get("prompt_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0),
                completion_tokens: u
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0),
                total_tokens: u
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0),
            }
        }
        ChannelProvider::Anthropic => {
            let u = body.get("usage");
            let input = u
                .and_then(|u| u.get("input_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let output = u
                .and_then(|u| u.get("output_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            Usage {
                prompt_tokens: input,
                completion_tokens: output,
                total_tokens: input + output,
            }
        }
    }
}

/// Swap the `model` field on a request body to the upstream name.
fn swap_model(body: &mut serde_json::Value, upstream_model_name: &str) {
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".to_string(), serde_json::Value::String(upstream_model_name.to_string()));
    }
}

/// Build the outgoing request body for a given key + entry format.
///
/// - Same format: just swap `model`.
/// - Cross-format: translate via chennix-translator, then swap `model`.
///
/// Returns `(body, adaptor_provider)` so the caller knows which response
/// format to expect.
fn prepare_request(
    entry_format: EntryFormat,
    body: serde_json::Value,
    channel: &chennix_common::ChannelConfig,
    upstream_model_name: &str,
) -> ProxyResult<(serde_json::Value, ChannelProvider)> {
    let adaptor_provider = channel.provider;
    let mut out = match (entry_format, adaptor_provider) {
        (EntryFormat::OpenAI, ChannelProvider::OpenaiCompatible) => body,
        (EntryFormat::Claude, ChannelProvider::Anthropic) => body,
        (EntryFormat::OpenAI, ChannelProvider::Anthropic) => {
            chennix_translator::o2c::openai_to_claude_request(&body)?
        }
        (EntryFormat::Claude, ChannelProvider::OpenaiCompatible) => {
            chennix_translator::c2o::claude_to_openai_request(&body)?
        }
    };
    swap_model(&mut out, upstream_model_name);
    Ok((out, adaptor_provider))
}

/// Translate an upstream response back to the client's entry format.
fn translate_response_back(
    entry_format: EntryFormat,
    adaptor_provider: ChannelProvider,
    body: serde_json::Value,
) -> ProxyResult<serde_json::Value> {
    match (entry_format, adaptor_provider) {
        (EntryFormat::OpenAI, ChannelProvider::OpenaiCompatible) => Ok(body),
        (EntryFormat::Claude, ChannelProvider::Anthropic) => Ok(body),
        (EntryFormat::OpenAI, ChannelProvider::Anthropic) => {
            chennix_translator::o2c::claude_to_openai_response(&body)
        }
        (EntryFormat::Claude, ChannelProvider::OpenaiCompatible) => {
            chennix_translator::c2o::openai_to_claude_response(&body)
        }
    }
}

/// Pick the right adaptor instance for a channel. Adaptors are stateless
/// so we construct a fresh one per call — cheaper than caching them.
fn pick_adaptor(provider: ChannelProvider) -> Box<dyn Adaptor> {
    match provider {
        ChannelProvider::OpenaiCompatible => Box::new(OpenaiAdaptor::new()),
        ChannelProvider::Anthropic => Box::new(ClaudeAdaptor::new()),
    }
}

/// Bootstrap result returned by `execute_stream`. The upstream has accepted
/// the request and started streaming; the caller (HTTP handler) is now
/// responsible for:
/// 1. Forwarding chunks from `response` to the client.
/// 2. Extracting usage per chunk via the appropriate adaptor.
/// 3. Settling billing via `BillingManager::settle` using `session` once
///    the stream completes (or refunding on error).
/// 4. Tracking usage via `Tracker::track_success`.
pub struct StreamBootstrap {
    pub response: reqwest::Response,
    pub session: crate::billing::BillingSession,
    pub routed_key: RoutedKey,
    /// Per-binding pricing (model_id + channel_id). Used by the
    /// streaming handler to compute `actual_cost`.
    pub model_pricing: Option<ChannelModelPricing>,
    /// The wire format the client used to talk to us.
    pub entry_format: EntryFormat,
    /// The upstream channel's native format.
    pub adaptor_provider: ChannelProvider,
}

impl std::fmt::Debug for StreamBootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamBootstrap")
            .field("response_status", &self.response.status().as_u16())
            .field("session", &self.session)
            .field("routed_key", &self.routed_key)
            .field("entry_format", &self.entry_format)
            .field("adaptor_provider", &self.adaptor_provider)
            .finish()
    }
}

impl Executor {
    pub fn new(
        health: Arc<HealthManager>,
        cache: Arc<ConfigCache>,
        http_client: reqwest::Client,
        upstream_timeout: std::time::Duration,
        streaming_timeout: std::time::Duration,
    ) -> Self {
        Self {
            health,
            cache,
            http_client,
            upstream_timeout,
            streaming_timeout,
        }
    }

    /// Resolve the routed candidate keys for a request, in the order the
    /// executor should attempt them. Extracted as a standalone method so
    /// tests can verify routing without spinning up upstream HTTP servers.
    pub async fn select_keys(
        &self,
        ctx: &ExecutionContext,
        cache_loader: &dyn crate::cache::CacheLoader,
    ) -> ProxyResult<Vec<RoutedKey>> {
        // NOTE: do NOT call `check_recoveries()` here.
        //
        // `is_available` already checks `cooldown_until > Utc::now()` inline,
        // so cooldown recovery happens lazily per-key without the O(N) write
        // lock traversal `check_recoveries` would require. Background task in
        // main.rs runs `check_recoveries` every 10s to reset
        // `consecutive_failures` (which only affects backoff window length)
        // and to roll over small-model quota windows — neither is on the
        // hot path.
        //
        // 对齐 new-api relay.go：恢复由后台任务触发，不在每请求路径跑
        // 全量写锁遍历，避免高并发下的锁竞争 + O(N) 遍历 + DB 写放大。
        let tuples = self
            .cache
            .get_for_model(ctx.model_id, &ctx.user_group, cache_loader)
            .await?;
        let strategy = self
            .cache
            .routing_strategy_for(ctx.model_id, cache_loader)
            .await
            .unwrap_or(RoutingStrategy::Priority);
        let health_for_keys = self.health.clone();
        let health_for_quota = self.health.clone();
        let routed = Router::route(
            tuples,
            &ctx.user_group,
            move |key_id| {
                // Synchronous availability check. Uses try_read on the
                // HealthManager's internal RwLock — if contended, returns
                // true (optimistic) and the per-key async check in the
                // executor loop re-validates.
                health_for_keys.try_is_available(key_id)
            },
            strategy,
            // Small-model quota filter: drop bindings whose
            // `(channel_id, upstream)` quota is exhausted. Uses the same
            // try_read / optimistic-fallback contract as `try_is_available`.
            move |channel_id, upstream| {
                health_for_quota.is_small_model_available(channel_id, upstream)
            },
        );
        Ok(routed)
    }

    /// Non-streaming execution. See module docs for the full flow.
    pub async fn execute(
        &self,
        ctx: &ExecutionContext,
        entry_format: EntryFormat,
        body: serde_json::Value,
        billing_repo: &dyn BillingRepo,
        usage_writer: &dyn UsageWriter,
        cache_loader: &dyn crate::cache::CacheLoader,
    ) -> ProxyResult<Bytes> {
        let candidates = self.select_keys(ctx, cache_loader).await?;
        if candidates.is_empty() {
            return Err(ProxyError::AllKeysExhausted {
                model: ctx.canonical_name.clone(),
                attempted_keys: Vec::new(),
                last_error: None,
            });
        }

        // Pre-charge against the *first* candidate's key — this is the key
        // we expect to use. If we end up using a different key, settle will
        // still true up against actual usage; the per-binding price only
        // affects the *actual_cost* computation, not the pre-charge.
        let model_pricing = self
            .cache
            .get_channel_model_pricing(
                ctx.model_id,
                candidates[0].channel.id,
                &candidates[0].upstream_model_name,
                cache_loader,
            )
            .await?;
        let max_tokens = extract_max_tokens(&body, entry_format);
        let estimated = estimate_cost(model_pricing.as_ref(), max_tokens);
        let mut session = BillingManager::pre_charge(
            billing_repo, ctx.user_id, ctx.token_id, estimated,
        )
        .await?;

        let mut attempted: Vec<String> = Vec::new();
        let mut last_error: Option<String> = None;

        for rk in &candidates {
            // Re-check availability (async, authoritative).
            if !self.health.is_available(rk.key.id).await {
                continue;
            }

            let label = rk
                .key
                .label
                .clone()
                .unwrap_or_else(|| format!("key-{}", rk.key.id));
            attempted.push(label);

            let (req_body, adaptor_provider) = match prepare_request(
                entry_format,
                body.clone(),
                &rk.channel,
                &rk.upstream_model_name,
            ) {
                Ok(v) => v,
                Err(e) => {
                    // Translator error — surface to client, refund.
                    BillingManager::refund(billing_repo, session).await?;
                    return Err(e);
                }
            };

            let adaptor = pick_adaptor(rk.channel.provider);
            // 用 tokio::time::timeout 包裹整体非流式请求，超时则视为可重试的临时故障。
            let exec_fut = adaptor.execute(
                &self.http_client,
                &rk.channel.base_url,
                &rk.key.api_key,
                req_body,
                HashMap::new(),
            );
            let exec_result = tokio::time::timeout(self.upstream_timeout, exec_fut)
                .await
                .map_err(|_| {
                    tracing::warn!(
                        key_id = rk.key.id,
                        timeout_secs = self.upstream_timeout.as_secs(),
                        "upstream request timed out"
                    );
                    ProxyError::UpstreamTimeout(self.upstream_timeout)
                })?;
            match exec_result {
                Ok((_status, bytes)) => {
                    // Extract usage from the upstream-native response.
                    let usage_value: serde_json::Value =
                        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
                    let usage =
                        extract_usage_from_response(&usage_value, adaptor_provider);
                    // 实际命中的渠道可能不同于预扣时的第一个候选，需重新取该绑定的定价。
                    // 三元组定价下，channel 相同但 upstream 不同也算不同绑定，需一并比较。
                    let actual_pricing = if rk.channel.id == candidates[0].channel.id
                        && rk.upstream_model_name == candidates[0].upstream_model_name
                    {
                        model_pricing.clone()
                    } else {
                        self.cache
                            .get_channel_model_pricing(
                                ctx.model_id,
                                rk.channel.id,
                                &rk.upstream_model_name,
                                cache_loader,
                            )
                            .await?
                    };
                    let cost = actual_cost(&usage, actual_pricing.as_ref());

                    // Settle billing against actual usage.
                    // 对齐 new-api PostTextConsumeQuota：响应已从上游拿到，
                    // 计费结算失败只记日志不传播（不能因记账失败丢响应）。
                    if let Err(e) =
                        BillingManager::settle(billing_repo, &mut session, cost).await
                    {
                        tracing::error!(
                            "billing settle failed (response already obtained): {}",
                            e
                        );
                    }

                    // Track success (durable + runtime).
                    // 对齐 new-api RecordConsumeLog：best-effort，失败只记日志。
                    if let Err(e) = Tracker::track_success(
                        usage_writer,
                        &self.health,
                        &self.cache,
                        ctx.user_id,
                        ctx.token_id,
                        rk.channel.id,
                        rk.key.id,
                        ctx.model_id,
                        &rk.upstream_model_name,
                        &usage,
                        cost,
                        "chat",
                    )
                    .await
                    {
                        tracing::error!("track_success failed: {}", e);
                    }

                    // Translate response back if cross-format.
                    let final_body = if adaptor_provider == entry_format.provider() {
                        bytes
                    } else {
                        let translated =
                            translate_response_back(entry_format, adaptor_provider, usage_value)?;
                        Bytes::from(serde_json::to_vec(&translated)?)
                    };
                    return Ok(final_body);
                }
                Err(e) => {
                    let err_str = e.to_string();
                    last_error = Some(err_str.clone());
                    // 记录本次 key 尝试的失败（best-effort，不影响后续重试/返回）。
                    let _ = Tracker::track_failure(
                        usage_writer,
                        ctx.user_id,
                        ctx.token_id,
                        rk.channel.id,
                        rk.key.id,
                        ctx.model_id,
                        "chat",
                        &err_str,
                    )
                    .await;
                    match classify_failure(&e) {
                        FailureAction::ReturnToClient => {
                            // 400/422 — bad request. Refund + return.
                            BillingManager::refund(billing_repo, session).await?;
                            return Err(e);
                        }
                        FailureAction::Disable => {
                            self.health.mark_disabled(rk.key.id).await;
                            continue;
                        }
                        FailureAction::Cooldown => {
                            self.health.mark_cooldown(rk.key.id).await;
                            continue;
                        }
                    }
                }
            }
        }

        // All keys exhausted. Refund the pre-charge.
        BillingManager::refund(billing_repo, session).await?;
        Err(ProxyError::AllKeysExhausted {
            model: ctx.canonical_name.clone(),
            attempted_keys: attempted,
            last_error,
        })
    }

    /// Streaming execution. The executor establishes the upstream
    /// connection (bootstrap phase) and returns a `StreamBootstrap` once
    /// the upstream has accepted the request. Per-chunk forwarding,
    /// usage extraction, billing settlement, and tracking happen in the
    /// HTTP handler (Task 25/26).
    pub async fn execute_stream(
        &self,
        ctx: &ExecutionContext,
        entry_format: EntryFormat,
        body: serde_json::Value,
        billing_repo: &dyn BillingRepo,
        usage_writer: &dyn UsageWriter,
        cache_loader: &dyn crate::cache::CacheLoader,
    ) -> ProxyResult<StreamBootstrap> {
        let candidates = self.select_keys(ctx, cache_loader).await?;
        if candidates.is_empty() {
            return Err(ProxyError::AllKeysExhausted {
                model: ctx.canonical_name.clone(),
                attempted_keys: Vec::new(),
                last_error: None,
            });
        }

        let model_pricing = self
            .cache
            .get_channel_model_pricing(
                ctx.model_id,
                candidates[0].channel.id,
                &candidates[0].upstream_model_name,
                cache_loader,
            )
            .await?;
        let max_tokens = extract_max_tokens(&body, entry_format);
        let estimated = estimate_cost(model_pricing.as_ref(), max_tokens);
        let session = BillingManager::pre_charge(
            billing_repo, ctx.user_id, ctx.token_id, estimated,
        )
        .await?;

        let mut attempted: Vec<String> = Vec::new();
        let mut last_error: Option<String> = None;

        for rk in &candidates {
            if !self.health.is_available(rk.key.id).await {
                continue;
            }

            let label = rk
                .key
                .label
                .clone()
                .unwrap_or_else(|| format!("key-{}", rk.key.id));
            attempted.push(label);

            let (req_body, adaptor_provider) = match prepare_request(
                entry_format,
                body.clone(),
                &rk.channel,
                &rk.upstream_model_name,
            ) {
                Ok(v) => v,
                Err(e) => {
                    BillingManager::refund(billing_repo, session).await?;
                    return Err(e);
                }
            };

            let adaptor = pick_adaptor(rk.channel.provider);
            // 首字节超时：仅覆盖建立连接 + 等待上游开始流式响应的阶段。
            // 一旦 adaptor 返回 Ok(resp) 即视为已建立流，后续 chunk 转发不受此超时约束。
            let exec_fut = adaptor.execute_stream(
                &self.http_client,
                &rk.channel.base_url,
                &rk.key.api_key,
                req_body,
                HashMap::new(),
            );
            let exec_result = tokio::time::timeout(self.streaming_timeout, exec_fut)
                .await
                .map_err(|_| {
                    tracing::warn!(
                        key_id = rk.key.id,
                        timeout_secs = self.streaming_timeout.as_secs(),
                        "streaming first-byte timed out"
                    );
                    ProxyError::UpstreamTimeout(self.streaming_timeout)
                })?;
            match exec_result {
                Ok(resp) => {
                    // Bootstrap boundary crossed: the upstream has accepted
                    // the request and is starting to stream. We are now
                    // committed — return the StreamBootstrap to the caller.
                    // The caller (HTTP handler) will stream chunks to the
                    // client, extract usage, settle billing, and track
                    // success.
                    //
                    // NOTE: billing is NOT settled here; the caller must do
                    // it once the stream finishes (or errors out).
                    return Ok(StreamBootstrap {
                        response: resp,
                        session,
                        routed_key: rk.clone(),
                        model_pricing,
                        entry_format,
                        adaptor_provider,
                    });
                }
                Err(e) => {
                    let err_str = e.to_string();
                    last_error = Some(err_str.clone());
                    // 记录本次 key 尝试的失败（best-effort，不影响后续重试/返回）。
                    let _ = Tracker::track_failure(
                        usage_writer,
                        ctx.user_id,
                        ctx.token_id,
                        rk.channel.id,
                        rk.key.id,
                        ctx.model_id,
                        "chat",
                        &err_str,
                    )
                    .await;
                    match classify_failure(&e) {
                        FailureAction::ReturnToClient => {
                            BillingManager::refund(billing_repo, session).await?;
                            return Err(e);
                        }
                        FailureAction::Disable => {
                            self.health.mark_disabled(rk.key.id).await;
                            continue;
                        }
                        FailureAction::Cooldown => {
                            self.health.mark_cooldown(rk.key.id).await;
                            continue;
                        }
                    }
                }
            }
        }

        // All keys failed during bootstrap. Refund + error.
        BillingManager::refund(billing_repo, session).await?;
        Err(ProxyError::AllKeysExhausted {
            model: ctx.canonical_name.clone(),
            attempted_keys: attempted,
            last_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chennix_common::{
        BillingType, ChannelConfig, ChannelModelPricing, ChannelProvider, CostTier, KeyConfig,
        KeyStatus, Usage,
    };
    use crate::cache::{Binding, CacheData, CacheLoader};
    use crate::normalizer::Normalizer;
    use std::collections::HashMap;
    use async_trait::async_trait;

    // ---------- helper builders ----------

    fn channel(id: i64, group: &str, provider: ChannelProvider) -> ChannelConfig {
        ChannelConfig {
            id,
            name: format!("ch-{id}"),
            provider,
            base_url: format!("http://127.0.0.1:1/ch-{id}"), // non-routable: forces failure
            group: group.into(),
        }
    }

    fn key(id: i64, channel_id: i64, tier: CostTier, kp: i32) -> KeyConfig {
        KeyConfig {
            id,
            channel_id,
            api_key: format!("sk-{id}"),
            label: Some(format!("label-{id}")),
            cost_tier: tier,
            key_priority: kp,
            price_per_1k_tokens: Some(0.01),
            free_quota: Some(100_000),
            used_quota: 0,
            quota_reset_period: None,
            status: KeyStatus::Active,
        }
    }

    fn ctx(model_id: i64) -> ExecutionContext {
        ExecutionContext {
            user_id: 1,
            token_id: 10,
            user_group: "default".into(),
            model_id,
            canonical_name: format!("model-{model_id}"),
        }
    }

    /// Loader that returns a fixed snapshot with two channels bound to
    /// model 7: a Free/priority-50 channel and a Paid/priority-100 channel.
    fn loader_with_two_keys() -> impl CacheLoader {
        struct L;
        #[async_trait]
        impl CacheLoader for L {
            async fn load_all(&self) -> ProxyResult<CacheData> {
                let ch1 = channel(1, "default", ChannelProvider::OpenaiCompatible);
                let ch2 = channel(2, "default", ChannelProvider::OpenaiCompatible);
                let k1 = key(10, 1, CostTier::Paid, 100);
                let k2 = key(11, 2, CostTier::Free, 100);
                let mut keys = HashMap::new();
                keys.insert(1, vec![k1]);
                keys.insert(2, vec![k2]);
                let mut bindings = HashMap::new();
                bindings.insert(
                    7,
                    vec![
                        Binding {
                            channel_id: 1,
                            upstream_model_name: "up-a".into(),
                            priority: 100,
                            weight: 1,
                        },
                        Binding {
                            channel_id: 2,
                            upstream_model_name: "up-b".into(),
                            priority: 50,
                            weight: 1,
                        },
                    ],
                );
                let mut channel_model_pricing = HashMap::new();
                channel_model_pricing.insert(
                    (7, 1, "up-a".to_string()),
                    ChannelModelPricing {
                        billing_type: BillingType::Token,
                        input_price: 0.03,
                        output_price: 0.06,
                        call_price: 0.0,
                        billing_expr: None,
                    },
                );
                Ok(CacheData {
                    channels: vec![ch1, ch2],
                    keys,
                    bindings,
                    channel_model_pricing,
                    ..Default::default()
                })
            }
            async fn load_alias_mapping(&self) -> ProxyResult<HashMap<String, (i64, String)>> {
                Ok(HashMap::new())
            }
        }
        L
    }

    fn make_executor() -> Executor {
        let normalizer = Arc::new(Normalizer::new());
        let cache = Arc::new(ConfigCache::new(normalizer));
        let health = Arc::new(HealthManager::new());
        Executor::new(
            health,
            cache,
            reqwest::Client::new(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(300),
        )
    }

    // ---------- error classification tests ----------

    #[test]
    fn test_classify_return_to_client_for_invalid_request() {
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 400, body: "".into() }),
            FailureAction::ReturnToClient
        );
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 422, body: "".into() }),
            FailureAction::ReturnToClient
        );
        assert_eq!(
            classify_failure(&ProxyError::InvalidRequest("bad".into())),
            FailureAction::ReturnToClient
        );
    }

    #[test]
    fn test_classify_disable_for_fatal() {
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 401, body: "".into() }),
            FailureAction::Disable
        );
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 403, body: "".into() }),
            FailureAction::Disable
        );
    }

    #[test]
    fn test_classify_cooldown_for_retryable_and_network() {
        // 429 + 5xx
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 429, body: "".into() }),
            FailureAction::Cooldown
        );
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 500, body: "".into() }),
            FailureAction::Cooldown
        );
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 503, body: "".into() }),
            FailureAction::Cooldown
        );
        // 404 is not in any of the three categories → cooldown (try next key)
        assert_eq!(
            classify_failure(&ProxyError::Upstream { status: 404, body: "".into() }),
            FailureAction::Cooldown
        );
    }

    // ---------- cost helpers ----------

    fn token_pricing(input: f64, output: f64) -> ChannelModelPricing {
        ChannelModelPricing {
            billing_type: BillingType::Token,
            input_price: input,
            output_price: output,
            call_price: 0.0,
            billing_expr: None,
        }
    }

    fn percall_pricing(call_price: f64) -> ChannelModelPricing {
        ChannelModelPricing {
            billing_type: BillingType::PerCall,
            input_price: 0.0,
            output_price: 0.0,
            call_price,
            billing_expr: None,
        }
    }

    fn expr_pricing(expr: &str) -> ChannelModelPricing {
        ChannelModelPricing {
            billing_type: BillingType::Expression,
            input_price: 0.0,
            output_price: 0.0,
            call_price: 0.0,
            billing_expr: Some(expr.into()),
        }
    }

    #[test]
    fn test_estimate_cost_no_pricing_free() {
        // No pricing supplied → free (cost = 0)
        assert_eq!(estimate_cost(None, None), 0);
        // Pricing present but not configured (all zeros, no expr) → free
        let p = ChannelModelPricing::default();
        assert_eq!(estimate_cost(Some(&p), None), 0);
    }

    #[test]
    fn test_estimate_cost_token_mode() {
        // estimate_cost assumes 500 prompt + 500 completion tokens (default).
        // input: 500/1000 * 0.03 = 0.015
        // output: 500/1000 * 0.06 = 0.03
        // total = 0.045 → rounds to 0, but .max(1) → 1
        let p = token_pricing(0.03, 0.06);
        assert_eq!(estimate_cost(Some(&p), None), 1);

        // Larger prices: 500 * 10/1000 + 500 * 20/1000 = 5 + 10 = 15
        let p = token_pricing(10.0, 20.0);
        assert_eq!(estimate_cost(Some(&p), None), 15);
    }

    #[test]
    fn test_estimate_cost_token_mode_with_max_tokens() {
        // With max_tokens=2000: 500 * 10/1000 + 2000 * 20/1000 = 5 + 40 = 45
        let p = token_pricing(10.0, 20.0);
        assert_eq!(estimate_cost(Some(&p), Some(2000)), 45);
    }

    #[test]
    fn test_estimate_cost_percall_mode() {
        // Per-call estimate = call_price, min 1.
        let p = percall_pricing(0.5);
        assert_eq!(estimate_cost(Some(&p), None), 1);

        let p = percall_pricing(7.0);
        assert_eq!(estimate_cost(Some(&p), None), 7);
    }

    #[test]
    fn test_estimate_cost_expression_mode() {
        // Expression uses assumed 500/500 tokens. `p + c` → 1000.
        let p = expr_pricing("p + c");
        assert_eq!(estimate_cost(Some(&p), None), 1000);

        // Fixed per-call style via expression: constant.
        let p = expr_pricing("3");
        assert_eq!(estimate_cost(Some(&p), None), 3);

        // Tiered via `if(cond, then, else)` builtin: total = 1000 > 500 → 10.
        let p = expr_pricing("if(total > 500, 10, 2)");
        assert_eq!(estimate_cost(Some(&p), None), 10);
    }

    #[test]
    fn test_estimate_cost_expression_mode_with_max_tokens() {
        // max_tokens=1000 → p=500, c=1000, total=1500 > 500 → 10
        let p = expr_pricing("if(total > 500, 10, 2)");
        assert_eq!(estimate_cost(Some(&p), Some(1000)), 10);
    }

    #[test]
    fn test_extract_max_tokens_openai() {
        // OpenAI: max_completion_tokens takes precedence
        let body = serde_json::json!({"max_tokens": 100, "max_completion_tokens": 200});
        assert_eq!(extract_max_tokens(&body, EntryFormat::OpenAI), Some(200));

        // Only max_tokens
        let body = serde_json::json!({"max_tokens": 100});
        assert_eq!(extract_max_tokens(&body, EntryFormat::OpenAI), Some(100));

        // Neither
        let body = serde_json::json!({"messages": []});
        assert_eq!(extract_max_tokens(&body, EntryFormat::OpenAI), None);

        // Zero or negative → None
        let body = serde_json::json!({"max_tokens": 0});
        assert_eq!(extract_max_tokens(&body, EntryFormat::OpenAI), None);
    }

    #[test]
    fn test_extract_max_tokens_claude() {
        // Claude: only max_tokens
        let body = serde_json::json!({"max_tokens": 1024});
        assert_eq!(extract_max_tokens(&body, EntryFormat::Claude), Some(1024));

        // Absent
        let body = serde_json::json!({"messages": []});
        assert_eq!(extract_max_tokens(&body, EntryFormat::Claude), None);
    }

    #[test]
    fn test_actual_cost_no_pricing_free() {
        let u = Usage { prompt_tokens: 1000, completion_tokens: 500, total_tokens: 1500 };
        // No pricing → free (cost = 0)
        assert_eq!(actual_cost(&u, None), 0);
        // Pricing present but not configured → free
        let p = ChannelModelPricing::default();
        assert_eq!(actual_cost(&u, Some(&p)), 0);
    }

    #[test]
    fn test_actual_cost_token_mode() {
        let p = token_pricing(0.03, 0.06);
        // Small usage → rounds to 0
        let u = Usage { prompt_tokens: 1000, completion_tokens: 500, total_tokens: 1500 };
        // input: 1000/1000 * 0.03 = 0.03
        // output: 500/1000 * 0.06 = 0.03
        // total = 0.06 → 0
        assert_eq!(actual_cost(&u, Some(&p)), 0);

        // Larger usage → non-zero
        let u = Usage { prompt_tokens: 100_000, completion_tokens: 50_000, total_tokens: 150_000 };
        // input: 100000/1000 * 0.03 = 3.0
        // output: 50000/1000 * 0.06 = 3.0
        // total = 6.0 → 6
        assert_eq!(actual_cost(&u, Some(&p)), 6);
    }

    #[test]
    fn test_actual_cost_percall_mode() {
        let p = percall_pricing(2.0);
        let u = Usage { prompt_tokens: 1000, completion_tokens: 500, total_tokens: 1500 };
        // Per-call: call_price regardless of tokens → 2
        assert_eq!(actual_cost(&u, Some(&p)), 2);
    }

    #[test]
    fn test_actual_cost_expression_mode() {
        // p + c
        let p = expr_pricing("p + c");
        let u = Usage { prompt_tokens: 1000, completion_tokens: 500, total_tokens: 1500 };
        // 1000 + 500 = 1500
        assert_eq!(actual_cost(&u, Some(&p)), 1500);

        // Tiered by total via `if(cond, then, else)` builtin
        let p = expr_pricing("if(total > 1000, total * 0.01, 5)");
        let u = Usage { prompt_tokens: 1000, completion_tokens: 500, total_tokens: 1500 };
        // total=1500 > 1000 → 1500 * 0.01 = 15
        assert_eq!(actual_cost(&u, Some(&p)), 15);

        let u = Usage { prompt_tokens: 100, completion_tokens: 100, total_tokens: 200 };
        // total=200 ≤ 1000 → 5
        assert_eq!(actual_cost(&u, Some(&p)), 5);
    }

    // ---------- prepare_request / translate_response_back ----------

    #[test]
    fn test_prepare_request_same_format_swaps_model() {
        let ch = channel(1, "default", ChannelProvider::OpenaiCompatible);
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let (out, provider) =
            prepare_request(EntryFormat::OpenAI, body, &ch, "gpt-4-upstream").unwrap();
        assert_eq!(provider, ChannelProvider::OpenaiCompatible);
        assert_eq!(out["model"], "gpt-4-upstream");
        // body otherwise unchanged
        assert!(out.get("messages").is_some());
    }

    #[test]
    fn test_prepare_request_cross_format_translates_and_swaps_model() {
        // OpenAI entry → Claude adaptor: should run O2C and end up with a
        // Claude-style body (messages as array of {role, content:[{type:"text"}]}).
        let ch = channel(1, "default", ChannelProvider::Anthropic);
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100
        });
        let (out, provider) =
            prepare_request(EntryFormat::OpenAI, body, &ch, "claude-upstream").unwrap();
        assert_eq!(provider, ChannelProvider::Anthropic);
        assert_eq!(out["model"], "claude-upstream");
        // Claude format requires messages[].content be an array
        let msgs = out["messages"].as_array().unwrap();
        assert!(!msgs.is_empty());
        assert!(msgs[0]["content"].is_array());
        // max_tokens is required for Claude and should pass through
        assert_eq!(out["max_tokens"], 100);
    }

    #[test]
    fn test_translate_response_back_same_format_passthrough() {
        let body = serde_json::json!({"choices": [{"message": {"content": "hi"}}]});
        let out = translate_response_back(
            EntryFormat::OpenAI,
            ChannelProvider::OpenaiCompatible,
            body.clone(),
        )
        .unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn test_translate_response_back_cross_format() {
        // Claude response → OpenAI: should produce {object: "chat.completion", ...}
        let claude_resp = serde_json::json!({
            "id": "msg_1",
            "model": "claude",
            "content": [{"type": "text", "text": "Hello!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        });
        let out = translate_response_back(
            EntryFormat::OpenAI,
            ChannelProvider::Anthropic,
            claude_resp,
        )
        .unwrap();
        assert_eq!(out["object"], "chat.completion");
        assert_eq!(out["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(out["usage"]["total_tokens"], 8);
    }

    // ---------- usage extraction ----------

    #[test]
    fn test_extract_usage_openai() {
        let body = serde_json::json!({
            "id": "x",
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let u = extract_usage_from_response(&body, ChannelProvider::OpenaiCompatible);
        assert_eq!(u.total_tokens, 15);
    }

    #[test]
    fn test_extract_usage_claude() {
        let body = serde_json::json!({
            "id": "msg_1",
            "usage": {"input_tokens": 7, "output_tokens": 4}
        });
        let u = extract_usage_from_response(&body, ChannelProvider::Anthropic);
        assert_eq!(u.prompt_tokens, 7);
        assert_eq!(u.completion_tokens, 4);
        assert_eq!(u.total_tokens, 11);
    }

    #[test]
    fn test_extract_usage_missing_field_returns_zero() {
        let body = serde_json::json!({"id": "x"});
        let u = extract_usage_from_response(&body, ChannelProvider::OpenaiCompatible);
        assert_eq!(u.total_tokens, 0);
    }

    // ---------- select_keys: route ordering ----------

    #[tokio::test]
    async fn test_select_keys_orders_by_tier_then_priority() {
        // Loader returns ch1 (Paid, priority 100) and ch2 (Free, priority 50).
        // Expected order: ch2's key first (Free + lower priority number),
        // then ch1's key.
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let ctx = ctx(7);

        let keys = exec.select_keys(&ctx, &loader).await.unwrap();
        assert_eq!(keys.len(), 2);
        // Free key (id=11) first, Paid (id=10) second
        assert_eq!(keys[0].key.id, 11);
        assert_eq!(keys[1].key.id, 10);
        // upstream names propagated
        assert_eq!(keys[0].upstream_model_name, "up-b");
        assert_eq!(keys[1].upstream_model_name, "up-a");
    }

    #[tokio::test]
    async fn test_select_keys_skips_unavailable_keys() {
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let ctx = ctx(7);

        // Mark key 11 (would be first) as cooldown.
        exec.health.mark_cooldown(11).await;
        let keys = exec.select_keys(&ctx, &loader).await.unwrap();
        // Only key 10 remains — key 11 was filtered out by the sync
        // availability predicate inside Router::route.
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key.id, 10);
    }

    #[tokio::test]
    async fn test_select_keys_unknown_model_returns_empty() {
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let mut ctx = ctx(7);
        ctx.model_id = 999;
        let keys = exec.select_keys(&ctx, &loader).await.unwrap();
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn test_select_keys_group_filter_excludes_other_groups() {
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let mut ctx = ctx(7);
        ctx.user_group = "vip".into(); // channels are in "default" group only
        let keys = exec.select_keys(&ctx, &loader).await.unwrap();
        assert!(keys.is_empty(), "vip user should not see default-group channels");
    }

    // ---------- execute: error paths ----------

    /// Billing repo that always reports ample quota. Used to isolate
    /// executor decision logic from actual quota checks.
    struct NoopBilling;
    #[async_trait]
    impl BillingRepo for NoopBilling {
        async fn get_user_quota(&self, _: i64) -> ProxyResult<Option<i64>> { Ok(Some(i64::MAX)) }
        async fn get_token_remain_quota(&self, _: i64) -> ProxyResult<Option<i64>> { Ok(Some(i64::MAX)) }
        async fn update_token_status(&self, _: i64, _: i32) -> ProxyResult<()> { Ok(()) }
        async fn get_token_unlimited(&self, _: i64) -> ProxyResult<Option<bool>> { Ok(Some(false)) }
        async fn pre_charge_atomic(&self, _: i64, _: i64, _: i64, _: bool) -> ProxyResult<()> { Ok(()) }
        async fn settle_atomic(&self, _: i64, _: i64, _: i64, _: bool) -> ProxyResult<()> { Ok(()) }
        async fn refund_atomic(&self, _: i64, _: i64, _: i64, _: bool) -> ProxyResult<()> { Ok(()) }
    }

    struct NoopWriter;
    #[async_trait]
    impl UsageWriter for NoopWriter {
        async fn log_usage(&self, _: i64, _: i64, _: i64, _: i64, _: i64, _: &Usage, _: i64, _: &str, _: &str, _: Option<&str>) -> ProxyResult<()> { Ok(()) }
        async fn add_key_usage(&self, _: i64, _: u64) -> ProxyResult<()> { Ok(()) }
        async fn add_small_model_usage(&self, _: i64, _: &str, _: i64, _: &str) -> ProxyResult<()> { Ok(()) }
        async fn log_request(&self, _: &str, _: Option<&str>, _: &str, _: &str, _: Option<&str>, _: Option<&str>, _: Option<&str>, _: Option<&str>, _: Option<&str>, _: Option<i64>, _: i64, _: i64, _: bool, _: Option<&str>, _: Option<i64>, _: Option<i64>, _: i64) -> ProxyResult<()> { Ok(()) }
    }

    #[tokio::test]
    async fn test_execute_no_candidates_returns_all_keys_exhausted() {
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let mut ctx = ctx(7);
        ctx.model_id = 999; // no bindings
        let body = serde_json::json!({"model": "x", "messages": []});

        let err = exec
            .execute(&ctx, EntryFormat::OpenAI, body, &NoopBilling, &NoopWriter, &loader)
            .await
            .unwrap_err();
        assert!(matches!(err, ProxyError::AllKeysExhausted { .. }), "got {:?}", err);
    }

    #[tokio::test]
    async fn test_execute_all_keys_unreachable_marks_cooldown_and_exhausts() {
        // Both channels point at non-routable 127.0.0.1:1 — adaptor will
        // return a network (Http) error, which classify_failure treats as
        // Cooldown. After both keys fail, executor returns AllKeysExhausted
        // and both keys should be in Cooldown state.
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let ctx = ctx(7);
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 10
        });

        let err = exec
            .execute(&ctx, EntryFormat::OpenAI, body, &NoopBilling, &NoopWriter, &loader)
            .await
            .unwrap_err();

        match err {
            ProxyError::AllKeysExhausted { attempted_keys, .. } => {
                // both keys were attempted
                assert_eq!(attempted_keys.len(), 2, "both keys should be tried");
            }
            other => panic!("expected AllKeysExhausted, got {:?}", other),
        }

        // Both keys should now be in Cooldown (or Disabled — but our
        // network error path uses Cooldown).
        for id in [10, 11] {
            let s = exec.health.get_state(id).await.expect("state must exist");
            assert!(
                s.status == KeyStatus::Cooldown,
                "key {} should be in Cooldown, got {:?}",
                id,
                s.status
            );
        }
    }

    #[tokio::test]
    async fn test_execute_stream_no_candidates_returns_all_keys_exhausted() {
        let exec = make_executor();
        let loader = loader_with_two_keys();
        let mut ctx = ctx(7);
        ctx.model_id = 999;
        let body = serde_json::json!({"model": "x", "messages": []});

        let err = exec
            .execute_stream(&ctx, EntryFormat::OpenAI, body, &NoopBilling, &NoopWriter, &loader)
            .await
            .unwrap_err();
        assert!(matches!(err, ProxyError::AllKeysExhausted { .. }));
    }
}

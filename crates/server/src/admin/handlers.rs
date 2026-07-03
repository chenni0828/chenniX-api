//! Admin API handlers: dashboard, CRUD for users/tokens/channels/keys/models,
//! usage summary, request logs, and cache reload.
//!
//! Every handler returns `Result<T, AdminError>` where `T: IntoResponse`.
//! Storage errors are auto-converted via `From<ProxyError> for AdminError`.

use axum::extract::{Path, Query, State};
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};

use chennix_common::{
    AdminAuthContext, ChannelConfig, ChannelModelPricing, ChannelProvider, ConnectionTestResult,
    DashboardOverview, KeyConfig, ModelUsage, RequestLog, TokenConfig, TokenUsageStats,
    UsageSummary, UserConfig,
};
use chennix_storage::channels::{ChannelRepo, DiscoveredModelRepo, DiscoveredModelWithCount};
use chennix_storage::keys::KeyRepo;
use chennix_storage::models::ModelRepo;
use chennix_storage::tokens::TokenRepo;
use chennix_storage::usage::UsageRepo;
use chennix_storage::users::UserRepo;

use chennix_adaptor::{build_claude_messages_url, build_models_url, build_openai_chat_url};
use chennix_core::cache::CacheLoader;
use crate::admin::error::{AdminError, AdminResult};
use crate::state::AppState;

// ======================================================================
// Dashboard
// ======================================================================

/// `GET /admin/api/dashboard` — combined dashboard data.
///
/// **管理员专用**：仪表盘包含全局统计（所有用户的 token 用量、请求数、
/// 错误数、活跃 key 数、热门模型、最近请求），普通用户无权访问。
/// 普通用户可通过 `/admin/api/usage` 和 `/admin/api/logs` 查看自己的数据。
#[derive(Debug, Serialize)]
pub struct DashboardResponse {
    pub overview: DashboardOverview,
    pub top_models: Vec<ModelUsage>,
    pub recent_requests: Vec<RequestLog>,
}

pub async fn dashboard_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
) -> AdminResult<Json<DashboardResponse>> {
    if !auth.user.is_admin() {
        return Err(AdminError::Forbidden(
            "dashboard is admin-only".into(),
        ));
    }
    let db = state.db.lock().await;
    let repo = UsageRepo::new(&db);
    let overview = repo.get_dashboard_overview()?;
    let top_models = repo.get_top_models(10)?;
    let recent_requests = repo.get_recent_requests(10)?;
    Ok(Json(DashboardResponse {
        overview,
        top_models,
        recent_requests,
    }))
}

// ======================================================================
// Users CRUD
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: i32,
    pub group: String,
    pub quota: i64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub username: String,
    pub role: i32,
    pub status: i32,
    pub group: String,
    pub quota: i64,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePasswordRequest {
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMyPasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

/// `GET /admin/api/users`
pub async fn list_users_handler(State(state): State<AppState>) -> AdminResult<Json<Vec<UserConfig>>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    Ok(Json(repo.list_users()?))
}

/// `POST /admin/api/users`
pub async fn create_user_handler(
    State(state): State<AppState>,
    Json(payload): Json<CreateUserRequest>,
) -> AdminResult<Json<i64>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    let hash = bcrypt::hash(&payload.password, bcrypt::DEFAULT_COST)
        .map_err(|e| AdminError::Internal(format!("bcrypt hash failed: {}", e)))?;
    let id = repo.create_user_with_quota(
        &payload.username,
        &hash,
        payload.role,
        &payload.group,
        payload.quota,
    )?;
    Ok(Json(id))
}

/// `PUT /admin/api/users/:id`
pub async fn update_user_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateUserRequest>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    repo.update_user(
        id,
        &payload.username,
        payload.role,
        payload.status,
        &payload.group,
        payload.quota,
    )?;
    Ok(Json(()))
}

/// `DELETE /admin/api/users/:id`
pub async fn delete_user_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    repo.delete_user(id)?;
    Ok(Json(()))
}

/// `PUT /admin/api/users/:id/password` 鈥?change password only (admin).
pub async fn update_password_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdatePasswordRequest>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    let hash = bcrypt::hash(&payload.password, bcrypt::DEFAULT_COST)
        .map_err(|e| AdminError::Internal(format!("bcrypt hash failed: {}", e)))?;
    repo.update_password(id, &hash)?;
    Ok(Json(()))
}

/// `PUT /admin/api/me/password` 鈥?self-service password change.
///
/// Any logged-in user can change their own password. The old password must
/// be provided and verified before the new password is accepted.
pub async fn update_my_password_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Json(payload): Json<UpdateMyPasswordRequest>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);

    // Verify the old password.
    let hash = repo
        .get_password_hash(&auth.user.username)
        .map_err(AdminError::from)?
        .ok_or_else(|| AdminError::Internal("password hash missing".into()))?;

    let valid = bcrypt::verify(&payload.old_password, &hash).unwrap_or(false);
    if !valid {
        return Err(AdminError::BadRequest("old password is incorrect".into()));
    }

    // Hash and save the new password.
    let new_hash = bcrypt::hash(&payload.new_password, bcrypt::DEFAULT_COST)
        .map_err(|e| AdminError::Internal(format!("bcrypt hash failed: {}", e)))?;
    repo.update_password(auth.user.id, &new_hash)?;
    Ok(Json(()))
}

// ======================================================================
// Tokens CRUD
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct ListTokensQuery {
    pub user_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    pub user_id: i64,
    pub name: String,
    pub key: Option<String>,
    pub remain_quota: i64,
    pub unlimited_quota: bool,
    pub expired_time: i64,
    pub model_limits: String,
    pub model_limits_enabled: bool,
    pub allow_ips: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTokenRequest {
    pub name: String,
    pub remain_quota: i64,
    pub unlimited_quota: bool,
    pub expired_time: i64,
    pub model_limits: String,
    pub model_limits_enabled: bool,
    pub allow_ips: String,
    pub status: i32,
}

/// `GET /admin/api/tokens?user_id=`
///
/// Non-admin users always see only their own tokens (the `user_id` query
/// parameter is ignored). Admins may pass `?user_id=` to filter by a specific
/// user, or omit it to list all tokens.
pub async fn list_tokens_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Query(query): Query<ListTokensQuery>,
) -> AdminResult<Json<Vec<TokenConfig>>> {
    // Non-admins are forced to their own user_id; admins may use the query param.
    let effective_user_id = if auth.user.is_admin() {
        query.user_id
    } else {
        Some(auth.user.id)
    };
    let db = state.db.lock().await;
    let repo = TokenRepo::new(&db);
    Ok(Json(repo.list_tokens(effective_user_id)?))
}

/// `POST /admin/api/tokens`
///
/// Creates a token. The token is automatically associated with the current
/// authenticated user. Admins may pass `?user_id=` to assign to a different user.
pub async fn create_token_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Query(query): Query<ListTokensQuery>,
    Json(payload): Json<CreateTokenRequest>,
) -> AdminResult<Json<i64>> {
    // Determine the target user_id:
    // - If the query has a user_id AND the current user is admin, use it.
    // - If the body has a non-zero user_id AND the current user is admin, use it.
    // - Otherwise, use the current authenticated user's id.
    let target_user_id = if let Some(qid) = query.user_id {
        if auth.user.is_admin() { qid } else { auth.user.id }
    } else if payload.user_id != 0 && auth.user.is_admin() {
        payload.user_id
    } else {
        auth.user.id
    };
    // Auto-generate key if not provided
    let key = payload.key.unwrap_or_else(|| {
        format!("sk-chennix-{}", &uuid::Uuid::new_v4().to_string().replace('-', "")[..16])
    });
    let db = state.db.lock().await;
    let repo = TokenRepo::new(&db);
    let id = repo.create_token_full(
        target_user_id,
        &payload.name,
        &key,
        payload.remain_quota,
        payload.unlimited_quota,
        payload.expired_time,
        &payload.model_limits,
        payload.model_limits_enabled,
        &payload.allow_ips,
    )?;
    Ok(Json(id))
}

/// `PUT /admin/api/tokens/:id`
///
/// Non-admin users may only update their own tokens. Admins may update any token.
pub async fn update_token_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateTokenRequest>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = TokenRepo::new(&db);
    // Ownership check: non-admins can only operate on their own tokens.
    if !auth.user.is_admin() {
        let token = repo
            .get_token_by_id(id)?
            .ok_or_else(|| AdminError::NotFound(format!("token {} not found", id)))?;
        if token.user_id != auth.user.id {
            return Err(AdminError::Forbidden(
                "you can only modify your own tokens".into(),
            ));
        }
    }
    repo.update_token(
        id,
        &payload.name,
        payload.remain_quota,
        payload.unlimited_quota,
        payload.expired_time,
        &payload.model_limits,
        payload.model_limits_enabled,
        &payload.allow_ips,
        payload.status,
    )?;
    Ok(Json(()))
}

/// `DELETE /admin/api/tokens/:id`
///
/// Non-admin users may only delete their own tokens. Admins may delete any token.
pub async fn delete_token_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Path(id): Path<i64>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = TokenRepo::new(&db);
    // Ownership check: non-admins can only operate on their own tokens.
    if !auth.user.is_admin() {
        let token = repo
            .get_token_by_id(id)?
            .ok_or_else(|| AdminError::NotFound(format!("token {} not found", id)))?;
        if token.user_id != auth.user.id {
            return Err(AdminError::Forbidden(
                "you can only delete your own tokens".into(),
            ));
        }
    }
    repo.delete_token_by_id(id)?;
    Ok(Json(()))
}

// ======================================================================
// Channels CRUD
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
    pub provider: ChannelProvider,
    pub base_url: String,
    pub group: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateChannelRequest {
    pub name: String,
    pub provider: ChannelProvider,
    pub base_url: String,
    pub group: String,
}

/// `GET /admin/api/channels`
pub async fn list_channels_handler(
    State(state): State<AppState>,
) -> AdminResult<Json<Vec<ChannelConfig>>> {
    let db = state.db.lock().await;
    let repo = ChannelRepo::new(&db);
    Ok(Json(repo.list_channels()?))
}

/// `POST /admin/api/channels`
pub async fn create_channel_handler(
    State(state): State<AppState>,
    Json(payload): Json<CreateChannelRequest>,
) -> AdminResult<Json<i64>> {
    let db = state.db.lock().await;
    let repo = ChannelRepo::new(&db);
    let id = repo.create_channel_full(
        &payload.name,
        &payload.provider,
        &payload.base_url,
        &payload.group,
    )?;
    Ok(Json(id))
}

/// `PUT /admin/api/channels/:id`
pub async fn update_channel_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateChannelRequest>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = ChannelRepo::new(&db);
    repo.update_channel(
        id,
        &payload.name,
        &payload.provider,
        &payload.base_url,
        &payload.group,
    )?;
    Ok(Json(()))
}

/// `DELETE /admin/api/channels/:id`
pub async fn delete_channel_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = ChannelRepo::new(&db);
    repo.delete_channel(id)?;
    Ok(Json(()))
}

// ======================================================================
// Keys CRUD (nested under channels)
// ======================================================================

/// Request body for creating a key.
///
/// API-level field names (`is_free`, `priority`, `quota_limit`) are mapped
/// to the database column names (`cost_tier`, `key_priority`, `free_quota`)
/// inside the storage layer's `create_key_full` method:
/// - `is_free: bool`  鈫?`cost_tier: "free" | "paid"`
/// - `priority: i32`  鈫?`key_priority: i32`
/// - `quota_limit: i64` 鈫?`free_quota: Option<i64>` (None when 0)
#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub api_key: String,
    pub label: Option<String>,
    pub is_free: bool,
    pub priority: i32,
    pub quota_limit: i64,
    pub price_per_1k_tokens: f64,
}

/// Request body for updating a key (same field mapping as `CreateKeyRequest`).
///
/// `status` uses string encoding: `"active"`, `"disabled"`, `"cooldown"`, `"quota_exhausted"`.
#[derive(Debug, Deserialize)]
pub struct UpdateKeyRequest {
    pub api_key: String,
    pub is_free: bool,
    pub priority: i32,
    pub quota_limit: i64,
    pub price_per_1k_tokens: f64,
    pub status: String,
}

/// `GET /admin/api/channels/:id/keys`
pub async fn list_keys_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
) -> AdminResult<Json<Vec<KeyConfig>>> {
    let db = state.db.lock().await;
    let repo = KeyRepo::new(&db);
    Ok(Json(repo.get_keys_for_channel(channel_id)?))
}

/// `POST /admin/api/channels/:id/keys`
pub async fn create_key_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    Json(payload): Json<CreateKeyRequest>,
) -> AdminResult<Json<i64>> {
    let db = state.db.lock().await;
    let repo = KeyRepo::new(&db);
    let id = repo.create_key_full(
        channel_id,
        &payload.api_key,
        payload.label.as_deref(),
        payload.is_free,
        payload.priority,
        payload.quota_limit,
        payload.price_per_1k_tokens,
    )?;
    Ok(Json(id))
}

/// `PUT /admin/api/channels/:id/keys/:kid`
pub async fn update_key_handler(
    State(state): State<AppState>,
    Path((channel_id, key_id)): Path<(i64, i64)>,
    Json(payload): Json<UpdateKeyRequest>,
) -> AdminResult<Json<()>> {
    let _ = channel_id; // channel_id is in the path for REST correctness but not needed for the update
    let status_i32 = match payload.status.as_str() {
        "active" => 1,
        "disabled" => 2,
        "cooldown" => 3,
        "quota_exhausted" => 4,
        _ => {
            return Err(AdminError::BadRequest(format!(
                "invalid status '{}': expected one of active, disabled, cooldown, quota_exhausted",
                payload.status
            )));
        }
    };
    let db = state.db.lock().await;
    let repo = KeyRepo::new(&db);
    repo.update_key(
        key_id,
        &payload.api_key,
        payload.is_free,
        payload.priority,
        payload.quota_limit,
        payload.price_per_1k_tokens,
        status_i32,
    )?;
    Ok(Json(()))
}

/// `DELETE /admin/api/channels/:id/keys/:kid`
pub async fn delete_key_handler(
    State(state): State<AppState>,
    Path((channel_id, key_id)): Path<(i64, i64)>,
) -> AdminResult<Json<()>> {
    let _ = channel_id;
    let db = state.db.lock().await;
    let repo = KeyRepo::new(&db);
    repo.delete_key(key_id)?;
    Ok(Json(()))
}

/// `POST /admin/api/channels/:id/keys/:kid/reset-quota` 鈥?manually reset a key's used_quota.
pub async fn reset_key_quota_handler(
    State(state): State<AppState>,
    Path((channel_id, key_id)): Path<(i64, i64)>,
) -> AdminResult<Json<serde_json::Value>> {
    let db = state.db.lock().await;
    let key_repo = KeyRepo::new(&db);
    key_repo.reset_key_quota(key_id, channel_id)?;
    Ok(Json(serde_json::json!({ "success": true, "message": "Quota reset" })))
}

// ======================================================================
// Models CRUD
// ======================================================================

/// Lightweight model info for the admin list view.
#[derive(Debug, Serialize)]
pub struct ModelInfo {
    pub id: i64,
    pub canonical_name: String,
    /// `'priority'` 或 `'load_balance'`。
    pub routing_strategy: String,
    pub bindings: Vec<ModelBindingInfo>,
}

#[derive(Debug, Serialize)]
pub struct ModelBindingInfo {
    pub channel_id: i64,
    pub channel_name: String,
    pub upstream_model_name: String,
    pub priority: i32,
    /// 负载均衡权重（>=1，仅 `load_balance` 策略生效）。
    pub weight: i32,
}

/// 完整 model 对象（用于 `create_model` 响应）。
#[derive(Debug, Serialize)]
pub struct ModelDetail {
    pub id: i64,
    pub canonical_name: String,
    pub routing_strategy: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateModelRequest {
    pub canonical_name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateModelRequest {
    pub canonical_name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRoutingStrategyRequest {
    pub strategy: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBindingPricingRequest {
    pub channel_id: i64,
    pub upstream_model_name: String,
    pub pricing: ChannelModelPricing,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBindingWeightRequest {
    pub channel_id: i64,
    pub upstream_model_name: String,
    pub weight: i32,
}

/// `GET /admin/api/models`
pub async fn list_models_handler(State(state): State<AppState>) -> AdminResult<Json<Vec<ModelInfo>>> {
    let db = state.db.lock().await;
    let repo = ModelRepo::new(&db);
    let ch_repo = ChannelRepo::new(&db);
    let all = repo.list_all_models()?;
    let mut result = Vec::with_capacity(all.len());
    for (id, canonical_name, routing_strategy) in &all {
        let bindings = repo.get_bindings_for_model(*id)?;
        let binding_infos: Vec<ModelBindingInfo> = bindings
            .into_iter()
            .map(|b| {
                let channel_name = ch_repo
                    .get_channel_by_id(b.channel_id)
                    .ok()
                    .flatten()
                    .map(|c| c.name)
                    .unwrap_or_default();
                ModelBindingInfo {
                    channel_id: b.channel_id,
                    channel_name,
                    upstream_model_name: b.upstream_model_name,
                    priority: b.priority,
                    weight: b.weight,
                }
            })
            .collect();
        result.push(ModelInfo {
            id: *id,
            canonical_name: canonical_name.clone(),
            routing_strategy: routing_strategy.clone(),
            bindings: binding_infos,
        });
    }
    Ok(Json(result))
}

/// `POST /admin/api/models`
///
/// 仅创建 `models` 行（`routing_strategy` 走 schema 默认值 `'priority'`），
/// 返回完整 model 对象（含 `id`、`canonical_name`、`routing_strategy`）。
pub async fn create_model_handler(
    State(state): State<AppState>,
    Json(payload): Json<CreateModelRequest>,
) -> AdminResult<Json<ModelDetail>> {
    let db = state.db.lock().await;
    let repo = ModelRepo::new(&db);
    let id = repo.create_model(&payload.canonical_name)?;
    Ok(Json(ModelDetail {
        id,
        canonical_name: payload.canonical_name,
        routing_strategy: "priority".to_string(),
    }))
}

/// `PATCH /admin/api/models/:id/strategy` — 切换大模型的路由策略。
///
/// body: `{ "strategy": "priority" | "load_balance" }`。
pub async fn update_routing_strategy_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateRoutingStrategyRequest>,
) -> AdminResult<Json<()>> {
    let strategy = payload.strategy.trim();
    if strategy != "priority" && strategy != "load_balance" {
        return Err(AdminError::BadRequest(format!(
            "invalid strategy '{}': expected 'priority' or 'load_balance'",
            payload.strategy
        )));
    }
    {
        let db = state.db.lock().await;
        let repo = ModelRepo::new(&db);
        repo.update_routing_strategy(id, strategy)?;
    }
    // Invalidate cache so the new strategy takes effect immediately.
    state.cache.invalidate().await;
    Ok(Json(()))
}

/// `PUT /admin/api/models/:id`
pub async fn update_model_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateModelRequest>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = ModelRepo::new(&db);
    repo.rename_model(id, &payload.canonical_name)?;
    Ok(Json(()))
}

/// `DELETE /admin/api/models/:id`
pub async fn delete_model_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AdminResult<Json<()>> {
    let db = state.db.lock().await;
    let repo = ModelRepo::new(&db);
    repo.delete_model(id)?;
    Ok(Json(()))
}

/// `PUT /admin/api/models/:id/pricing` — update per-binding pricing for a
/// `(model_id, channel_id, upstream_model_name)` triple. Pricing is
/// channel-model level: the same model may have different prices on
/// different channels / upstreams.
pub async fn update_binding_pricing_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateBindingPricingRequest>,
) -> AdminResult<Json<()>> {
    // Validate expression syntax before persisting — a malformed expression
    // would silently eval to 0 at request time (free service), bypassing billing.
    if let chennix_common::BillingType::Expression = payload.pricing.billing_type {
        if let Some(expr) = payload.pricing.billing_expr.as_deref() {
            chennix_core::billing_expr::validate(expr)
                .map_err(|e| AdminError::BadRequest(format!("invalid billing expression: {}", e)))?;
        } else {
            return Err(AdminError::BadRequest(
                "billing_expr is required for Expression mode".into(),
            ));
        }
    }
    {
        let db = state.db.lock().await;
        let repo = ModelRepo::new(&db);
        repo.update_binding_pricing(
            id,
            payload.channel_id,
            &payload.upstream_model_name,
            &payload.pricing,
        )?;
    }
    // Invalidate cache so the new pricing takes effect immediately.
    state.cache.invalidate().await;
    Ok(Json(()))
}

/// `GET /admin/api/pricing` — list all model-channel bindings with their
/// pricing configuration. Used by the dedicated Pricing management page.
pub async fn list_all_pricing_handler(
    State(state): State<AppState>,
) -> AdminResult<Json<Vec<chennix_storage::models::BindingPricingRow>>> {
    let db = state.db.lock().await;
    let repo = ModelRepo::new(&db);
    let rows = repo.list_all_bindings_with_pricing()?;
    Ok(Json(rows))
}

// ======================================================================
// Model Bindings
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct AddBindingRequest {
    pub channel_id: i64,
    pub upstream_model_name: String,
}

/// 单个绑定的排序项（三元组中的 channel + upstream 部分）。
#[derive(Debug, Deserialize)]
pub struct BindingOrderItem {
    pub channel_id: i64,
    pub upstream_model_name: String,
}

#[derive(Debug, Deserialize)]
pub struct ReorderBindingsRequest {
    /// 按调用优先级从高到低排列（索引 0 = 最高优先级）。
    pub bindings: Vec<BindingOrderItem>,
}

/// `POST /admin/api/models/:id/bindings` — bind a model to a channel.
///
/// 校验：`upstream_model_name` 必须等于被绑定 `discovered_model` 的
/// `raw_model_name`（查 `discovered_models` 表验证 `(channel_id,
/// raw_model_name)` 存在），不匹配返回 400。
///
/// 新绑定 priority：该大模型当前无绑定时默认 10，否则 `max(existing) + 10`。
/// `weight` 默认 1（仅 `load_balance` 策略生效）。
pub async fn add_binding_handler(
    State(state): State<AppState>,
    Path(model_id): Path<i64>,
    Json(payload): Json<AddBindingRequest>,
) -> AdminResult<Json<serde_json::Value>> {
    {
        let db = state.db.lock().await;
        let model_repo = ModelRepo::new(&db);
        let ch_repo = ChannelRepo::new(&db);
        let dm_repo = DiscoveredModelRepo::new(&db);
        // Verify model exists.
        model_repo
            .get_model_by_id(model_id)?
            .ok_or_else(|| AdminError::NotFound(format!("model {} not found", model_id)))?;
        // Verify channel exists.
        ch_repo
            .get_channel_by_id(payload.channel_id)?
            .ok_or_else(|| AdminError::NotFound(format!("channel {} not found", payload.channel_id)))?;
        // Verify the (channel_id, raw_model_name == upstream_model_name) row
        // exists in discovered_models — upstream_model_name must match a
        // discovered model's raw_model_name.
        if dm_repo
            .get_discovered_model(payload.channel_id, &payload.upstream_model_name)?
            .is_none()
        {
            return Err(AdminError::BadRequest(format!(
                "upstream_model_name '{}' does not match any discovered model \
                 (channel={}, raw_model_name) in discovered_models",
                payload.upstream_model_name, payload.channel_id
            )));
        }
        // New binding priority: 10 if no existing bindings, else max+10.
        let existing = model_repo.get_bindings_for_model(model_id)?;
        let priority = if existing.is_empty() {
            10
        } else {
            existing.iter().map(|b| b.priority).max().unwrap_or(0) + 10
        };
        // weight defaults to 1 (only affects load_balance strategy).
        model_repo.add_binding_with_weight(
            model_id,
            payload.channel_id,
            &payload.upstream_model_name,
            priority,
            1,
        )?;
    }
    // Invalidate cache so the new binding is immediately routable.
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `PATCH /admin/api/models/:id/bindings/weight` — update a binding's weight.
///
/// body: `{ "channel_id", "upstream_model_name", "weight" }`.
/// `weight` 必须 >= 1，否则返回 400。仅 `load_balance` 策略生效。
pub async fn update_binding_weight_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateBindingWeightRequest>,
) -> AdminResult<Json<()>> {
    if payload.weight < 1 {
        return Err(AdminError::BadRequest(format!(
            "weight must be >= 1 (got {})",
            payload.weight
        )));
    }
    {
        let db = state.db.lock().await;
        let repo = ModelRepo::new(&db);
        repo.update_binding_weight(
            id,
            payload.channel_id,
            &payload.upstream_model_name,
            payload.weight,
        )?;
    }
    // Invalidate cache so the new weight takes effect immediately.
    state.cache.invalidate().await;
    Ok(Json(()))
}

/// `DELETE /admin/api/models/:id/bindings/:channel_id/:upstream` — remove a
/// single `(model_id, channel_id, upstream_model_name)` triple binding.
/// `:upstream` is URL-decoded by axum automatically.
pub async fn remove_binding_handler(
    State(state): State<AppState>,
    Path((model_id, channel_id, upstream)): Path<(i64, i64, String)>,
) -> AdminResult<Json<serde_json::Value>> {
    {
        let db = state.db.lock().await;
        let repo = ModelRepo::new(&db);
        repo.remove_binding(model_id, channel_id, &upstream)?;
    }
    // Invalidate cache so the removed binding stops being routed.
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `PUT /admin/api/models/:id/bindings/reorder` — reorder binding priority.
///
/// Accepts the full ordered list of `(channel_id, upstream_model_name)`
/// bindings for this model. The handler reassigns priorities as
/// `(index + 1) * 10` so the caller's array order becomes the call order.
/// Runs in a transaction.
pub async fn reorder_bindings_handler(
    State(state): State<AppState>,
    Path(model_id): Path<i64>,
    Json(payload): Json<ReorderBindingsRequest>,
) -> AdminResult<Json<serde_json::Value>> {
    {
        let db = state.db.lock().await;
        let repo = ModelRepo::new(&db);
        let ordered: Vec<(i64, String)> = payload
            .bindings
            .into_iter()
            .map(|b| (b.channel_id, b.upstream_model_name))
            .collect();
        repo.reorder_bindings(model_id, &ordered)?;
    }
    // Invalidate cache so the new call order takes effect immediately.
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `POST /admin/api/channels/:id/test` 鈥?test connectivity for a channel.
///
/// Sends a real chat completion request using the first bound model on the
/// channel (or falls back to a basic `/v1/models` ping if no models are bound).
/// Supports both OpenAI-compatible and Anthropic providers.
/// Includes upstream response body in error messages for easier debugging.
pub async fn test_channel_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
) -> AdminResult<Json<ConnectionTestResult>> {
    // Scope the db lock so it is dropped before the HTTP request.
    let (channel, api_key, test_model) = {
        let db = state.db.lock().await;
        let ch_repo = ChannelRepo::new(&db);
        let channel = ch_repo
            .get_channel_by_id(channel_id)?
            .ok_or_else(|| AdminError::NotFound(format!("channel {} not found", channel_id)))?;
        let key_repo = KeyRepo::new(&db);
        let keys = key_repo.get_keys_for_channel(channel_id)?;
        let api_key = keys.first().map(|k| k.api_key.clone());
        // Try to find a bound model to use as the test model name.
        let model_repo = ModelRepo::new(&db);
        let test_model = model_repo
            .get_bindings_for_channel(channel_id)?
            .into_iter()
            .next()
            .map(|b| b.upstream_model_name);
        (channel, api_key, test_model)
    }; // db lock dropped here

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AdminError::Internal(format!("build http client: {}", e)))?;

    let start = std::time::Instant::now();

    if let Some(test_model) = &test_model {
        // Real chat completion test using the first bound model.
        let (url, body) = match channel.provider {
            ChannelProvider::Anthropic => {
                let url = build_claude_messages_url(&channel.base_url);
                let body = serde_json::json!({
                    "model": test_model,
                    "messages": [{"role": "user", "content": "hi"}],
                    "max_tokens": 1,
                });
                (url, body)
            }
            ChannelProvider::OpenaiCompatible => {
                let url = build_openai_chat_url(&channel.base_url);
                let body = serde_json::json!({
                    "model": test_model,
                    "messages": [{"role": "user", "content": "hi"}],
                    "max_tokens": 1,
                    "stream": false,
                });
                (url, body)
            }
        };

        let mut req = client.post(&url).json(&body);
        if let Some(key) = &api_key {
            match channel.provider {
                ChannelProvider::Anthropic => {
                    req = req
                        .header("x-api-key", key)
                        .header("anthropic-version", "2023-06-01");
                }
                ChannelProvider::OpenaiCompatible => {
                    req = req.bearer_auth(key);
                }
            }
        }

        match req.send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                let status = resp.status();
                if status.is_success() {
                    Ok(Json(ConnectionTestResult {
                        success: true,
                        latency_ms: latency,
                        error: None,
                    }))
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    Ok(Json(ConnectionTestResult {
                        success: false,
                        latency_ms: latency,
                        error: Some(format!("HTTP {}: {}", status, body)),
                    }))
                }
            }
            Err(e) => {
                let latency = start.elapsed().as_millis() as u64;
                Ok(Json(ConnectionTestResult {
                    success: false,
                    latency_ms: latency,
                    error: Some(e.to_string()),
                }))
            }
        }
    } else {
        // No bound models — fall back to basic connectivity check (GET /models).
        let url = build_models_url(&channel.base_url);
        let mut req = client.get(&url);
        if let Some(key) = &api_key {
            match channel.provider {
                ChannelProvider::Anthropic => {
                    req = req
                        .header("x-api-key", key)
                        .header("anthropic-version", "2023-06-01");
                }
                ChannelProvider::OpenaiCompatible => {
                    req = req.bearer_auth(key);
                }
            }
        }
        match req.send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                let status = resp.status();
                if status.is_success() {
                    Ok(Json(ConnectionTestResult {
                        success: true,
                        latency_ms: latency,
                        error: Some("no models bound - only basic connectivity verified".to_string()),
                    }))
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    Ok(Json(ConnectionTestResult {
                        success: false,
                        latency_ms: latency,
                        error: Some(format!("HTTP {}: {}", status, body)),
                    }))
                }
            }
            Err(e) => {
                let latency = start.elapsed().as_millis() as u64;
                Ok(Json(ConnectionTestResult {
                    success: false,
                    latency_ms: latency,
                    error: Some(e.to_string()),
                }))
            }
        }
    }
}

/// `POST /admin/api/models/:id/test` 鈥?test connectivity for a model.
///
/// Finds a channel that supports this model and sends a minimal chat
/// request to verify the full routing chain. Uses the appropriate API
/// format based on the channel's provider:
/// - OpenAI compatible: `POST /v1/chat/completions`
/// - Anthropic: `POST /v1/messages`
pub async fn test_model_handler(
    State(state): State<AppState>,
    Path(model_id): Path<i64>,
) -> AdminResult<Json<ConnectionTestResult>> {
    // Scope the db lock so it is dropped before the HTTP request.
    let (channel, api_key, upstream_model_name) = {
        let db = state.db.lock().await;
        let model_repo = ModelRepo::new(&db);

        // Verify the model exists.
        let _model = model_repo
            .get_model_by_id(model_id)?
            .ok_or_else(|| AdminError::NotFound(format!("model {} not found", model_id)))?;

        // Find a channel that supports this model.
        let bindings = model_repo.get_bindings_for_model(model_id)?;
        if bindings.is_empty() {
            return Ok(Json(ConnectionTestResult {
                success: false,
                latency_ms: 0,
                error: Some("no channels bound to this model".to_string()),
            }));
        }

        let binding = &bindings[0];
        let ch_repo = ChannelRepo::new(&db);
        let channel = ch_repo
            .get_channel_by_id(binding.channel_id)?
            .ok_or_else(|| {
                AdminError::NotFound(format!("channel {} not found", binding.channel_id))
            })?;

        let key_repo = KeyRepo::new(&db);
        let keys = key_repo.get_keys_for_channel(binding.channel_id)?;
        let api_key = keys.first().map(|k| k.api_key.clone());
        (channel, api_key, binding.upstream_model_name.clone())
    }; // db lock dropped here

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AdminError::Internal(format!("build http client: {}", e)))?;

    // Build the request based on provider type.
    let (url, body, auth_headers) = match channel.provider {
        ChannelProvider::OpenaiCompatible => {
            let url = build_openai_chat_url(&channel.base_url);
            let body = serde_json::json!({
                "model": upstream_model_name,
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1,
                "stream": false,
            });
            (url, body, "bearer")
        }
        ChannelProvider::Anthropic => {
            let url = build_claude_messages_url(&channel.base_url);
            let body = serde_json::json!({
                "model": upstream_model_name,
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1,
            });
            (url, body, "anthropic")
        }
    };

    let start = std::time::Instant::now();
    let mut req = client.post(&url).json(&body);
    if let Some(key) = &api_key {
        match auth_headers {
            "anthropic" => {
                req = req
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01");
            }
            _ => {
                req = req.bearer_auth(key);
            }
        }
    }
    let result = req.send().await;
    let latency = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                Ok(Json(ConnectionTestResult {
                    success: true,
                    latency_ms: latency,
                    error: None,
                }))
            } else {
                let body = resp.text().await.unwrap_or_default();
                Ok(Json(ConnectionTestResult {
                    success: false,
                    latency_ms: latency,
                    error: Some(format!("HTTP {}: {}", status, body)),
                }))
            }
        }
        Err(e) => Ok(Json(ConnectionTestResult {
            success: false,
            latency_ms: latency,
            error: Some(e.to_string()),
        })),
    }
}

/// `POST /admin/api/models/:id/bindings/:channel_id/test` 鈥?test a specific
/// model-channel binding.
///
/// Locates the (model_id, channel_id) binding, retrieves its
/// `upstream_model_name`, then sends a minimal chat request to that channel's
/// upstream API to verify the binding actually works end-to-end. Unlike
/// `test_model_handler` (which always picks the highest-priority binding),
/// this handler targets a specific binding so the admin can validate each
/// channel implementation independently.
pub async fn test_binding_handler(
    State(state): State<AppState>,
    Path((model_id, channel_id, upstream)): Path<(i64, i64, String)>,
) -> AdminResult<Json<ConnectionTestResult>> {
    // Scope the db lock so it is dropped before the HTTP request.
    let (channel, api_key, upstream_model_name) = {
        let db = state.db.lock().await;
        let model_repo = ModelRepo::new(&db);

        // Verify the model exists.
        let _model = model_repo
            .get_model_by_id(model_id)?
            .ok_or_else(|| AdminError::NotFound(format!("model {} not found", model_id)))?;

        // Find the specific binding for this channel + upstream. The triple
        // (model_id, channel_id, upstream_model_name) is the PK of model_channels,
        // so we must match on upstream too — otherwise a model with two bindings
        // to the same channel (different upstreams) would resolve to the first one.
        let binding = model_repo
            .get_bindings_for_model(model_id)?
            .into_iter()
            .find(|b| b.channel_id == channel_id && b.upstream_model_name == upstream)
            .ok_or_else(|| {
                AdminError::NotFound(format!(
                    "binding for model {} on channel {} (upstream {}) not found",
                    model_id, channel_id, upstream
                ))
            })?;

        let ch_repo = ChannelRepo::new(&db);
        let channel = ch_repo
            .get_channel_by_id(binding.channel_id)?
            .ok_or_else(|| {
                AdminError::NotFound(format!("channel {} not found", binding.channel_id))
            })?;

        let key_repo = KeyRepo::new(&db);
        let keys = key_repo.get_keys_for_channel(binding.channel_id)?;
        let api_key = keys.first().map(|k| k.api_key.clone());
        (channel, api_key, binding.upstream_model_name.clone())
    }; // db lock dropped here

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AdminError::Internal(format!("build http client: {}", e)))?;

    // Build the request based on provider type.
    let (url, body, auth_headers) = match channel.provider {
        ChannelProvider::OpenaiCompatible => {
            let url = build_openai_chat_url(&channel.base_url);
            let body = serde_json::json!({
                "model": upstream_model_name,
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1,
                "stream": false,
            });
            (url, body, "bearer")
        }
        ChannelProvider::Anthropic => {
            let url = build_claude_messages_url(&channel.base_url);
            let body = serde_json::json!({
                "model": upstream_model_name,
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1,
            });
            (url, body, "anthropic")
        }
    };

    let start = std::time::Instant::now();
    let mut req = client.post(&url).json(&body);
    if let Some(key) = &api_key {
        match auth_headers {
            "anthropic" => {
                req = req
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01");
            }
            _ => {
                req = req.bearer_auth(key);
            }
        }
    }
    let result = req.send().await;
    let latency = start.elapsed().as_millis() as u64;

    match result {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                Ok(Json(ConnectionTestResult {
                    success: true,
                    latency_ms: latency,
                    error: None,
                }))
            } else {
                let body = resp.text().await.unwrap_or_default();
                Ok(Json(ConnectionTestResult {
                    success: false,
                    latency_ms: latency,
                    error: Some(format!("HTTP {}: {}", status, body)),
                }))
            }
        }
        Err(e) => Ok(Json(ConnectionTestResult {
            success: false,
            latency_ms: latency,
            error: Some(e.to_string()),
        })),
    }
}

/// `GET /admin/api/channels/:id/models` 鈥?list models supported by a channel.
pub async fn list_channel_models_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
) -> AdminResult<Json<Vec<chennix_common::ChannelModelEntry>>> {
    let db = state.db.lock().await;
    let ch_repo = ChannelRepo::new(&db);

    // Verify channel exists.
    let _channel = ch_repo
        .get_channel_by_id(channel_id)?
        .ok_or_else(|| AdminError::NotFound(format!("channel {} not found", channel_id)))?;

    let models = ch_repo.get_channel_models(channel_id)?;

    Ok(Json(models))
}

// ======================================================================
// Channel Model Discovery & Management
// ======================================================================

/// `POST /admin/api/channels/:id/discover-models` 鈥?fetch available models from upstream.
///
/// Calls the upstream `/v1/models` endpoint using the channel's first active
/// key. Returns a list of model IDs. Supports both OpenAI-compatible and
/// Anthropic providers with the appropriate authentication headers.
pub async fn discover_channel_models_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
) -> AdminResult<Json<serde_json::Value>> {
    // Scope the db lock so it is dropped before the HTTP request.
    let (channel, api_key) = {
        let db = state.db.lock().await;
        let ch_repo = ChannelRepo::new(&db);
        let channel = ch_repo
            .get_channel_by_id(channel_id)?
            .ok_or_else(|| AdminError::NotFound(format!("channel {} not found", channel_id)))?;
        let key_repo = KeyRepo::new(&db);
        let keys = key_repo.get_keys_for_channel(channel_id)?;
        // Use the first Active key.
        let api_key = keys
            .into_iter()
            .find(|k| k.status.is_available())
            .map(|k| k.api_key);
        (channel, api_key)
    }; // db lock dropped here

    let api_key = api_key.ok_or_else(|| {
        AdminError::BadRequest("no active key found for this channel".into())
    })?;

    let url = build_models_url(&channel.base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AdminError::Internal(format!("build http client: {}", e)))?;

    let mut req = client.get(&url);
    match channel.provider {
        ChannelProvider::Anthropic => {
            req = req
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01");
        }
        ChannelProvider::OpenaiCompatible => {
            req = req.bearer_auth(&api_key);
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| AdminError::BadGateway(format!("request failed: {}", e)))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AdminError::BadGateway(format!(
            "Upstream HTTP {}: {}",
            status, body
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AdminError::BadGateway(format!("parse upstream response: {}", e)))?;

    // Parse OpenAI-style format: { "data": [{ "id": "model-name" }, ...] }
    let models: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // NOTE: Discovery is a pure probe — it only returns the upstream model list.
    // The caller must explicitly choose which models to add via
    // `add_discovered_models_handler` (POST /channels/:id/discovered-models),
    // which upserts into the discovered_models table. This restores the
    // "discover → select → add" flow that was lost when auto-upserting all
    // discovered models here.

    Ok(Json(serde_json::json!({ "models": models })))
}

/// Request body for bulk-adding discovered models to a channel's small-model pool.
#[derive(Debug, Deserialize)]
pub struct AddDiscoveredModelsRequest {
    pub models: Vec<String>,
}

/// `POST /admin/api/channels/:id/discovered-models` — bulk-add selected
/// discovered models into the `discovered_models` table (the small-model pool).
///
/// This is the second half of the "discover → select → add" flow:
/// `discover_channel_models_handler` only probes the upstream `/v1/models`
/// endpoint and returns the list; the caller then asks the user to select which
/// models to import, and submits that subset here. Existing rows are preserved
/// (upsert keeps their quota data); new rows are created with empty quota.
/// An empty `models` array is a no-op returning `added: 0` (not an error).
pub async fn add_discovered_models_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    Json(payload): Json<AddDiscoveredModelsRequest>,
) -> AdminResult<Json<serde_json::Value>> {
    // Filter out empty/whitespace-only names and dedupe (case-sensitive).
    let models: Vec<String> = payload
        .models
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if models.is_empty() {
        return Ok(Json(serde_json::json!({ "added": 0 })));
    }

    {
        let db = state.db.lock().await;
        // Verify the channel exists (404 if not).
        let ch_repo = ChannelRepo::new(&db);
        ch_repo
            .get_channel_by_id(channel_id)?
            .ok_or_else(|| AdminError::NotFound(format!("channel {} not found", channel_id)))?;

        let dm_repo = DiscoveredModelRepo::new(&db);
        for model_name in &models {
            dm_repo.upsert_discovered_model(
                channel_id,
                model_name,
                false,
                Some("manual"),
                None,
            )?;
        }
    }
    // Invalidate cache so the new discovered models appear in the small-model pool.
    state.cache.invalidate().await;

    Ok(Json(serde_json::json!({ "added": models.len() })))
}

/// Request body for form-based model discovery (no channel ID needed).
///
/// Used by the channel edit dialog to discover models before or after saving
/// the channel. The caller provides `base_url`, `api_key`, and `provider`
/// directly from the form.
#[derive(Debug, Deserialize)]
pub struct DiscoverModelsByFormRequest {
    pub base_url: String,
    pub api_key: String,
    pub provider: String,
}

/// `POST /admin/api/discover-models` 鈥?discover upstream models using form data.
///
/// Accepts `base_url`, `api_key`, and `provider` directly (no channel ID
/// required). Calls the upstream `/v1/models` endpoint and returns the list
/// of available model IDs. Supports both OpenAI-compatible and Anthropic
/// providers with the appropriate authentication headers.
pub async fn discover_models_by_form_handler(
    Json(payload): Json<DiscoverModelsByFormRequest>,
) -> AdminResult<Json<serde_json::Value>> {
    let provider = match payload.provider.as_str() {
        "anthropic" => ChannelProvider::Anthropic,
        _ => ChannelProvider::OpenaiCompatible,
    };

    let url = build_models_url(&payload.base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AdminError::Internal(format!("build http client: {}", e)))?;

    let mut req = client.get(&url);
    match provider {
        ChannelProvider::Anthropic => {
            req = req
                .header("x-api-key", &payload.api_key)
                .header("anthropic-version", "2023-06-01");
        }
        ChannelProvider::OpenaiCompatible => {
            req = req.bearer_auth(&payload.api_key);
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| AdminError::BadGateway(format!("request failed: {}", e)))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AdminError::BadGateway(format!(
            "Upstream HTTP {}: {}",
            status, body
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AdminError::BadGateway(format!("parse upstream response: {}", e)))?;

    // Parse OpenAI-style format: { "data": [{ "id": "model-name" }, ...] }
    let models: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("id").and_then(|id| id.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(serde_json::json!({ "models": models })))
}

#[derive(Debug, Deserialize)]
pub struct AddChannelModelRequest {
    pub model_name: String,
    pub upstream_model_name: String,
}

/// `POST /admin/api/channels/:id/models` 鈥?add a model to a channel.
///
/// Looks up `model_name` via canonical name (case-insensitive). If not found,
/// creates a new model with `canonical_name = model_name`. Then binds it to
/// the channel with the given `upstream_model_name`. The new binding is
/// appended to the end of the priority queue.
pub async fn add_channel_model_handler(
    State(state): State<AppState>,
    Path(channel_id): Path<i64>,
    Json(payload): Json<AddChannelModelRequest>,
) -> AdminResult<Json<serde_json::Value>> {
    let model_id = {
        let db = state.db.lock().await;
        let model_repo = ModelRepo::new(&db);
        let ch_repo = ChannelRepo::new(&db);

        // Verify channel exists.
        ch_repo
            .get_channel_by_id(channel_id)?
            .ok_or_else(|| AdminError::NotFound(format!("channel {} not found", channel_id)))?;

        // Find model by canonical_name; create if missing.
        let model_id = match model_repo.get_model_by_name(&payload.model_name)? {
            Some((id, _)) => id,
            None => model_repo.create_model(&payload.model_name)?,
        };

        // Append to the end of the priority queue.
        let existing = model_repo.get_bindings_for_model(model_id)?;
        let priority = (existing.len() as i32) * 10 + 10;
        model_repo.add_binding(model_id, channel_id, &payload.upstream_model_name, priority)?;
        model_id
    };
    // Invalidate cache so the new binding is immediately routable.
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true, "model_id": model_id })))
}

/// `DELETE /admin/api/channels/:id/models/:model_name` 鈥?remove a model from a channel.
///
/// Resolves `model_name` via canonical name (case-insensitive), then removes
/// the binding between that model and the channel.
pub async fn remove_channel_model_handler(
    State(state): State<AppState>,
    Path((channel_id, model_name)): Path<(i64, String)>,
) -> AdminResult<Json<serde_json::Value>> {
    {
        let db = state.db.lock().await;
        let model_repo = ModelRepo::new(&db);

        // Find model by canonical_name.
        let (model_id, _) = model_repo
            .get_model_by_name(&model_name)?
            .ok_or_else(|| AdminError::NotFound(format!("model '{}' not found", model_name)))?;

        // Remove ALL bindings between this model and the channel (a model
        // may be bound to the same channel via multiple upstreams under the
        // triple PK).
        model_repo.remove_all_bindings_for_channel(model_id, channel_id)?;
    }
    // Invalidate cache so the removed binding stops being routed.
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

// ======================================================================
// Small-Model Quota Management
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct UpdateSmallModelQuotaRequest {
    /// `None`（或 null）表示无限制。
    pub limit: Option<i64>,
    /// `'token'` | `'call'`。
    pub unit: String,
    /// `'day'` | `'month'` | `'total'`。
    pub window: String,
}

/// `GET /admin/api/small-models` — list all discovered small models with the
/// number of large-model bindings referencing each.
///
/// `binding_count` = 该 `(channel_id, raw_model_name)` 在 `model_channels`
/// 中作为 `(channel_id, upstream_model_name)` 出现的次数。
pub async fn list_small_models_handler(
    State(state): State<AppState>,
) -> AdminResult<Json<Vec<DiscoveredModelWithCount>>> {
    let db = state.db.lock().await;
    let repo = DiscoveredModelRepo::new(&db);
    Ok(Json(repo.list_discovered_models_with_binding_count()?))
}

/// `PATCH /admin/api/channels/:id/models/:upstream/quota` — configure a
/// small-model's quota. `:upstream` is the URL-decoded `raw_model_name`.
///
/// body: `{ "limit", "unit": "token"|"call", "window": "day"|"month"|"total" }`.
/// `limit = null` means unlimited. Resets `used_quota=0` and
/// `quota_status='available'`.
pub async fn update_small_model_quota_handler(
    State(state): State<AppState>,
    Path((channel_id, upstream)): Path<(i64, String)>,
    Json(payload): Json<UpdateSmallModelQuotaRequest>,
) -> AdminResult<Json<()>> {
    if payload.unit != "token" && payload.unit != "call" {
        return Err(AdminError::BadRequest(format!(
            "invalid unit '{}': expected 'token' or 'call'",
            payload.unit
        )));
    }
    if payload.window != "day" && payload.window != "month" && payload.window != "total" {
        return Err(AdminError::BadRequest(format!(
            "invalid window '{}': expected 'day', 'month' or 'total'",
            payload.window
        )));
    }
    {
        let db = state.db.lock().await;
        let repo = DiscoveredModelRepo::new(&db);
        repo.update_discovered_model_quota(
            channel_id,
            &upstream,
            payload.limit,
            Some(&payload.unit),
            Some(&payload.window),
        )?;
    }
    // Invalidate cache so the new quota config takes effect immediately
    // (full rebuild picks up the updated discovered_models row).
    state.cache.invalidate().await;
    Ok(Json(()))
}

/// `POST /admin/api/channels/:id/models/:upstream/quota/reset` — manually
/// reset a small-model's used quota. `:upstream` is the URL-decoded
/// `raw_model_name`. Sets `used_quota=0`, `quota_status='available'`.
pub async fn reset_small_model_quota_handler(
    State(state): State<AppState>,
    Path((channel_id, upstream)): Path<(i64, String)>,
) -> AdminResult<Json<serde_json::Value>> {
    {
        let db = state.db.lock().await;
        let repo = DiscoveredModelRepo::new(&db);
        repo.reset_discovered_model_quota(channel_id, &upstream)?;
    }
    // Invalidate cache so the reset state takes effect immediately.
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `DELETE /admin/api/channels/:id/discovered-models/:upstream`
///
/// 从小模型池移除指定发现模型。如果该模型已被大模型绑定
/// （`binding_count > 0`），返回 409 Conflict 阻止删除——
/// 调用方应先在 Models 页面解除绑定。
pub async fn delete_discovered_model_handler(
    State(state): State<AppState>,
    Path((channel_id, upstream)): Path<(i64, String)>,
) -> AdminResult<Json<serde_json::Value>> {
    {
        let db = state.db.lock().await;
        let repo = DiscoveredModelRepo::new(&db);
        // 检查是否已被大模型绑定
        let all = repo.list_discovered_models_with_binding_count()?;
        let target = all.into_iter().find(|d| {
            d.model.channel_id == channel_id && d.model.raw_model_name == upstream
        });
        if let Some(d) = target {
            if d.binding_count > 0 {
                return Err(AdminError::BadRequest(format!(
                    "该模型已被 {} 个大模型绑定，请先在 Models 页面解除绑定后再删除",
                    d.binding_count
                )));
            }
        }
        repo.delete_discovered_model(channel_id, &upstream)?;
    }
    state.cache.invalidate().await;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `GET /admin/api/tokens/:id/usage` 鈥?per-token consumption statistics.
pub async fn token_usage_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Path(token_id): Path<i64>,
) -> AdminResult<Json<TokenUsageStats>> {
    let db = state.db.lock().await;
    // 所有权校验：非管理员只能查询自己的 token 用量。
    if !auth.user.is_admin() {
        let token_repo = TokenRepo::new(&db);
        let token = token_repo
            .get_token_by_id(token_id)?
            .ok_or_else(|| AdminError::NotFound("token not found".into()))?;
        if token.user_id != auth.user.id {
            return Err(AdminError::Forbidden(
                "you do not own this token".into(),
            ));
        }
    }
    let repo = UsageRepo::new(&db);
    let stats = repo.get_token_usage_stats(token_id)?;
    Ok(Json(stats))
}

// ======================================================================
// Usage & Logs
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub channel_id: Option<i64>,
    pub model: Option<String>,
    pub start: Option<i64>,
    pub end: Option<i64>,
}

/// `GET /admin/api/usage`
///
/// Non-admin users only see their own usage. Admins see all usage.
pub async fn usage_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Query(query): Query<UsageQuery>,
) -> AdminResult<Json<Vec<UsageSummary>>> {
    // Non-admins are restricted to their own user_id.
    let user_filter = if auth.user.is_admin() {
        None
    } else {
        Some(auth.user.id)
    };
    let db = state.db.lock().await;
    let repo = UsageRepo::new(&db);
    let summary = repo.get_usage_summary(
        query.channel_id,
        query.model.as_deref(),
        query.start.unwrap_or(0),
        query.end.unwrap_or(0),
        user_filter,
    )?;
    Ok(Json(summary))
}

// ======================================================================
// Request Logs
// ======================================================================

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub channel_id: Option<i64>,
    pub model: Option<String>,
    pub status_code: Option<i32>,
    pub start: Option<i64>,
    pub end: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct LogsResponse {
    pub logs: Vec<RequestLog>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

/// `GET /admin/api/logs`
///
/// Non-admin users only see their own logs. Admins see all logs.
pub async fn logs_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AdminAuthContext>,
    Query(query): Query<LogsQuery>,
) -> AdminResult<Json<LogsResponse>> {
    let user_filter = if auth.user.is_admin() {
        None
    } else {
        Some(auth.user.id)
    };
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).clamp(1, 100);
    let db = state.db.lock().await;
    let repo = UsageRepo::new(&db);
    let (logs, total) = repo.get_request_logs(
        page,
        per_page,
        query.channel_id,
        query.model.as_deref(),
        query.status_code,
        query.start.unwrap_or(0),
        query.end.unwrap_or(0),
        user_filter,
    )?;
    Ok(Json(LogsResponse {
        logs,
        total,
        page,
        per_page,
    }))
}

// ======================================================================
// Cache Reload
// ======================================================================

/// `POST /admin/api/reload`
///
/// Forces a cache invalidation so the next request reloads from DB.
pub async fn reload_handler(
    State(state): State<AppState>,
) -> AdminResult<Json<serde_json::Value>> {
    state.cache.invalidate().await;
    let mapping = state.storage.load_alias_mapping().await?;
    state.normalizer.reload(mapping).await;
    Ok(Json(serde_json::json!({
        "success": true,
        "message": "Cache reloaded successfully"
    })))
}

// ======================================================================
// Discover Models (body-based)
// ======================================================================



# chenniX-api Code Wiki

> 一个轻量多用户的 AI API 代理网关，将多个 AI 提供商的 API 统一为 OpenAI / Claude 兼容格式对外暴露。
>
> - 语言/版本：Rust 2021（stable）
> - 工作区版本：0.1.0
> - 文档生成日期：2026-07-02

---

## 目录

1. [项目概述](#1-项目概述)
2. [技术栈](#2-技术栈)
3. [整体架构](#3-整体架构)
4. [项目结构与模块职责](#4-项目结构与模块职责)
5. [common：共享类型与错误](#5-common共享类型与错误)
6. [storage：SQLite 存储层](#6-storagesqlite-存储层)
7. [adaptor：上游适配器](#7-adaptor上游适配器)
8. [translator：跨格式转换](#8-translator跨格式转换)
9. [core：路由 / 健康 / 计费 / 执行](#9-core路由--健康--计费--执行)
10. [server：HTTP 服务层](#10-serverhttp-服务层)
11. [Web 管理面板（前端）](#11-web-管理面板前端)
12. [请求处理流程详解](#12-请求处理流程详解)
13. [错误处理与冷却机制](#13-错误处理与冷却机制)
14. [配置文件与运行方式](#14-配置文件与运行方式)
15. [测试策略](#15-测试策略)
16. [关键设计决策与借鉴](#16-关键设计决策与借鉴)

---

## 1. 项目概述

chenniX-api（包名前缀 `chennix-`）是一个用 Rust 实现的 AI API 代理网关，核心解决三个痛点：

1. **模型名归一化** — 同一模型在不同渠道商名字不一（`glm5.1`、`zhipu/GLM5.1`），代理统一映射，客户端只用标准名。
2. **多渠道优先级路由** — 同一模型挂多个渠道商，用户通过管理面板拖拽设置渠道和 Key 的优先级，失败自动重试、Key 级别冷却。
3. **跨格式转换** — 客户端用 OpenAI 格式可以调 Claude 后端，反之亦然（含流式状态机转换）。

### 多用户定位

- 管理员手动创建用户（无注册流程、无 OAuth）。
- **双层 Token 认证**：用户登录账号 → 创建多个 API Key（Token），每个 Token 可配独立额度和模型限制。
- **双层扣费**：用户总额度（银行账户）+ Token 独立额度（钱包），请求时预扣 → 结算（多退少补）。
- 渠道全局共享，通过用户分组（`group`）关联。
- 用量 / Token / 日志按 `user_id` 隔离，管理员可见全部。

### 对外端点

| 端点 | 方法 | 说明 |
|------|------|------|
| `/v1/chat/completions` | POST | OpenAI 兼容，支持 stream |
| `/v1/messages` | POST | Claude 兼容，支持 stream |
| `/v1/models` | GET | 返回可用模型列表（按 Token 的 model_limits 过滤） |
| `/admin/api/*` | * | 管理面板后端 API（session cookie 认证） |
| `/admin/*` | GET | 管理面板 SPA 静态文件 |
| `/health` | GET | 健康检查（无认证，供负载均衡探活） |

---

## 2. 技术栈

| 组件 | 选型 | 说明 |
|------|------|------|
| 语言 | Rust 2021 | 性能、类型安全 |
| HTTP 框架 | axum 0.7（features=`ws`） | async HTTP |
| 异步运行时 | tokio（features=`full`） | axum 标配 |
| HTTP 客户端 | reqwest 0.12（features=`stream`,`json`） | 流式响应支持 |
| 数据库 | rusqlite 0.31（features=`bundled`） | 单文件 SQLite，无需额外服务 |
| 序列化 | serde / serde_json / serde_yaml | 生态标准 |
| 日志 | tracing + tracing-subscriber | 结构化日志 |
| 中间件 | tower 0.4 + tower-http 0.5（cors, trace） | |
| 密码哈希 | bcrypt 0.15 | 管理员/用户密码 |
| 静态文件嵌入 | rust-embed 8 + mime_guess 2 | 单 binary 部署 |
| 前端 | React 19 + TypeScript + Vite 6 + Tailwind 4 + shadcn/ui + zustand + axios + recharts | 内嵌 SPA |

工作区依赖统一定义在根 [Cargo.toml](file:///c:/Users/chenniX/Desktop/chenniX-api/Cargo.toml) 的 `[workspace.dependencies]`，各 crate 通过 `workspace = true` 引用。

---

## 3. 整体架构

```
客户端请求
  │
  ▼
axum HTTP Server (:8080)
  ├─ /v1/chat/completions   (OpenAI 兼容)        ┐
  ├─ /v1/messages           (Claude 兼容)        ├─ token_auth_middleware
  └─ /v1/models             (模型列表)           ┘
  │
  ▼
请求处理管道 (routes/mod.rs::proxy_request):
  1. Auth Middleware        — Bearer Token → token+user 校验，注入 AuthContext
  2. Model Normalizer       — 模型名归一化 (别名 → 标准名, 大小写不敏感)
  3. ConfigCache            — 内存缓存渠道/Key/绑定 (lazy 加载, invalidate 重载)
  4. Channel & Key Router   — 按 user_group 过滤 → 展开为扁平 Key 列表 → 排序+过滤
  5. Billing pre_charge     — 双层预扣 (user.used_quota + token.remain_quota)
  6. execute_with_retry     — 遍历 Key:
        a. HealthManager.is_available (内存状态)
        b. prepare_request (同格式→替换 model / 跨格式→Translator 转换)
        c. Adaptor.execute / execute_stream (调上游)
        d. 三档错误分类: invalid_request→返回 / fatal(401/403)→disable / retryable(429/5xx)→cooldown
  7. Usage Tracker          — 记录 usage_logs + 累加 key.used_quota (内存+DB)
  8. Billing settle/refund  — 结算或退款
  │
  ▼
存储层:
  ├─ SQLite (chennix.db)  — 唯一 source of truth: 渠道/模型/用户/Token/用量/日志/健康状态
  └─ config.yaml       — 仅启动配置: 端口/日志级别/数据库路径/引导文件
```

### 分层职责边界

| Crate | 职责 | 依赖 |
|-------|------|------|
| `chennix-common` | 共享类型、错误定义 | 无 |
| `chennix-storage` | SQLite schema、各 Repo CRUD、引导导入 | common |
| `chennix-adaptor` | 上游 API 调用（Adaptor trait + OpenAI/Claude 实现） | common |
| `chennix-translator` | OpenAI ↔ Claude 格式转换（请求/响应/流式状态机） | common |
| `chennix-core` | 归一化、路由、执行循环、用量追踪、健康状态、双层计费、配置缓存 | common, storage, adaptor, translator |
| `chennix-server` | HTTP 路由、中间件、Web 面板、配置加载、静态文件 | 全部 crate |

依赖方向严格自上而下，`core` 不直接依赖 `server`，通过 trait（`CacheLoader`/`BillingRepo`/`UsageWriter`）解耦。

---

## 4. 项目结构与模块职责

```
chenniX-api/
├── Cargo.toml                 # workspace 定义 + 共享依赖
├── config.yaml                # 启动配置 (端口/日志/数据库/引导)
├── config.example.yaml        # 配置模板
├── bootstrap.example.yaml     # 首次启动引导模板 (models/channels/keys/bindings)
├── check_sizes.ps1            # 二进制体积检查脚本
├── docs/
│   ├── CODE_WIKI.md           # 本文档
│   └── superpowers/           # 设计文档与实现计划
│       ├── specs/2026-07-01-ai-api-proxy-design.md
│       └── plans/2026-07-01-mvp-core-proxy.md
└── crates/
    ├── common/                # 共享类型与错误
    ├── storage/               # SQLite 存储层
    ├── adaptor/               # 上游适配器
    ├── translator/            # 跨格式转换
    ├── core/                  # 核心逻辑层
    └── server/                # HTTP 服务 + 前端
        ├── src/
        ├── tests/             # 集成测试 (wiremock mock 上游)
        └── web/               # React 前端源码 (构建产物输出到 static/)
```

---

## 5. common：共享类型与错误

[crates/common/src/lib.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/common/src/lib.rs) — 仅 re-export `error` 与 `types` 模块。

### 5.1 错误类型 — [error.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/common/src/error.rs)

```rust
pub type ProxyResult<T> = Result<T, ProxyError>;

pub enum ProxyError {
    ClientAuthFailed,                              // 401
    ModelNotFound(String),                         // 404
    AllKeysDisabled { model },                     // 503 — 所有 Key 已被标记不可用
    AllKeysCooldown { model, earliest_recovery },  // 503 — 所有 Key 冷却中
    AllKeysQuotaExhausted { model },               // 503 — 所有免费 Key 额度用完
    AllKeysExhausted { model, attempted_keys, last_error }, // 503 — Key 用尽但状态混合
    Upstream { status: u16, body: String },        // 上游 HTTP 错误
    InvalidRequest(String),                        // 400 — 客户端请求非法
    Translator(String),                            // 502 — 格式转换失败
    Storage(String),                               // 500
    Config(String),                                // 500
    Io(std::io::Error), Json(serde_json::Error), Http(reqwest::Error),
}
```

关键方法（错误三档分类的核心）：

| 方法 | 判定逻辑 |
|------|---------|
| `is_retryable()` | `Upstream` 且 status=429 或 5xx |
| `is_fatal()` | `Upstream` 且 status=401 或 403 |
| `is_invalid_request()` | `Upstream` 且 status=400/422，或 `InvalidRequest` |
| `http_status()` | 各变体到 HTTP 状态码的映射 |

### 5.2 共享类型 — [types.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/common/src/types.rs)

| 类型 | 说明 |
|------|------|
| `ChannelProvider` | 枚举 `OpenaiCompatible` / `Anthropic`（kebab-case 序列化） |
| `CostTier` | 枚举 `Free` / `Paid` |
| `KeyStatus` | 枚举 `Active` / `Cooldown` / `Disabled` / `QuotaExhausted`，`is_available()` 仅 Active 为真 |
| `Usage` | `prompt_tokens` / `completion_tokens` / `total_tokens`，`add()` 累加 |
| `ChannelConfig` | 渠道配置：`id, name, provider, base_url, priority, group`（group 为逗号分隔的用户分组白名单） |
| `KeyConfig` | 渠道 Key 配置：`id, channel_id, api_key, label, cost_tier, key_priority, price_per_1k_tokens, free_quota, used_quota, quota_reset_period, status` |
| `ModelBinding` | 模型-渠道绑定：`model_id, canonical_name, channel_id, upstream_model_name` |
| `ModelPricing` | 模型定价：`input_price, output_price`（元/1K tokens） |
| `UserConfig` | 用户：含 `is_enabled()` / `is_admin()`（role≥10）/ `remaining_quota()` |
| `TokenConfig` | Token：含 `is_expired(now)` / `allows_model(model)` / `allows_ip(ip)` |
| `AuthContext` | 代理认证上下文：`{ user: UserConfig, token: TokenConfig }` |
| `AdminAuthContext` | 管理面板认证上下文：`{ user: UserConfig }`（仅 user，cookie 认证） |
| `DashboardOverview` / `ModelUsage` / `RequestLog` / `UsageSummary` / `TokenUsageStats` / `ConnectionTestResult` / `ChannelModelEntry` | 管理面板响应类型 |

---

## 6. storage：SQLite 存储层

[crates/storage/src/lib.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/lib.rs)

```rust
pub fn open_db(path: &str) -> ProxyResult<Connection>
// 打开 SQLite，开启 WAL + foreign_keys，调用 init_db 建表
```

### 6.1 Schema — [schema.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/schema.rs)

`init_db(conn)` 创建全部 11 张表（10 张业务表 + `schema_meta`，`CREATE TABLE IF NOT EXISTS`，幂等）：

| 表 | 用途 |
|----|------|
| `models` | 标准模型（`canonical_name` 唯一，`routing_strategy`） |
| `users` | 用户（username 唯一，bcrypt 密码，role/status/quota/used_quota/group） |
| `tokens` | 用户的 API Key（key 唯一，remain_quota/used_quota/unlimited_quota/expired_time/model_limits/allow_ips/status），索引 `idx_tokens_user`、`idx_tokens_key` |
| `channels` | 渠道（name 唯一，provider/base_url/priority/group） |
| `channel_keys` | 渠道 Key（cost_tier/key_priority/price/free_quota/used_quota/quota_reset_period/status；`cooldown_until`/`consecutive_failures` 列已废弃——cooldown 现为 per-(key, upstream_model) 内存态，不再持久化），索引 `idx_channel_keys_channel` |
| `discovered_models` | 上游发现的原始模型（channel_id+raw_model_name 唯一，status=unmerged/merged，is_free/source/metadata） |
| `model_channels` | 模型-渠道多对多绑定（联合主键，含 upstream_model_name） |
| `usage_logs` | 用量记录（精确到 user/token/channel/key/model 级别，含 quota_cost） |
| `request_logs` | 请求日志（request_id/client_ip/状态码/耗时/stream/attempted_keys 等） |
| `key_usage_summary` | Key 周期用量汇总（联合主键 key_id+period_start） |

`run_migrations(conn)` — 校验数据库 `schema_version` 是否匹配 `CURRENT_SCHEMA_VERSION`。项目不做向后兼容，版本不匹配直接报错。全新库由 `init_db` 写入最新版本号。

### 6.2 各 Repo（均持有 `&'a Connection`，方法返回 `ProxyResult<T>`）

#### ChannelRepo — [channels.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/channels.rs)
渠道 CRUD：`create_channel` / `create_channel_full`（含 group）/ `get_channel_by_id` / `get_channel_by_name` / `list_channels` / `update_channel` / `update_group` / `delete_channel` / `get_channel_models`。

#### KeyRepo — [keys.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/keys.rs)
Key CRUD 与配额：`create_key` / `create_key_full` / `get_keys_for_channel` / `get_key_by_id` / `update_key` / `update_key_status` / `delete_key` / `get_disabled_key_ids`（启动恢复用）/ `add_key_usage`（原子累加 used_quota）/ `reset_daily_quota` / `reset_monthly_quota` / `reset_key_quota`。

#### ModelRepo — [models.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/models.rs)
模型/别名/绑定/定价管理：
- 模型：`create_model` / `rename_model` / `delete_model` / `get_model_by_name` / `get_model_by_id` / `get_model_row_by_id` / `list_all_models` / `list_all_models_with_pricing`
- 别名：`resolve_alias`（先查别名表再回退 canonical_name）/ `add_alias` / `remove_alias` / `update_alias_target`（别名指向可改）/ `get_aliases`
- 绑定：`add_binding`（upsert）/ `remove_binding` / `get_bindings_for_model` / `get_bindings_for_channel`
- 定价：`update_model_pricing` / `get_model_pricing`

#### UserRepo — [users.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/users.rs)
用户 CRUD：`create_user` / `create_user_with_quota` / `get_user_by_id` / `get_user_by_username` / `update_user` / `update_password` / `get_password_hash` / `update_quota` / `update_used_quota_delta`（原子增减）/ `update_status` / `delete_user` / `list_users` / `get_quota`。

#### TokenRepo — [tokens.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/tokens.rs)
Token CRUD 与完整校验：`create_token` / `create_token_full` / `get_token_by_key` / `get_token_by_id` / `get_tokens_for_user` / `list_tokens` / `update_token` / `update_remain_quota_delta` / `update_used_quota_delta` / `update_status` / `set_model_limits` / `get_remain_quota` / `delete_token`（带 user_id 防越权）/ `delete_token_by_id`（管理员用）。
- **`validate_token(key, client_ip) -> Option<AuthContext>`** — 完整校验链：key→token→user，校验 status、过期、IP 白名单、用户启用状态，返回 `{user, token}` 上下文。

#### UsageRepo — [usage.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/usage.rs)
用量与日志：
- 写入：`log_usage(...)`（含 user_id/token_id/quota_cost）/ `log_request(...)`
- 仪表盘：`get_dashboard_overview` / `get_top_models` / `get_recent_requests`
- 趋势：`get_all_usage(days)` / `get_user_usage(user_id, days)` / `get_daily_usage_series(days)`
- 查询：`get_usage_summary(...)` / `get_request_logs(page, ...)` / `get_user_request_logs(user_id, ...)`
- Token：`get_token_usage_stats(token_id)`

### 6.3 引导导入 — [bootstrap.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/storage/src/bootstrap.rs)
- `import_from_yaml(conn, path)` — 单事务导入 models+aliases → channels → keys → bindings（缺失引用报 `Config` 错误）。
- `is_db_empty(conn) -> bool` — channels 表为空时返回 true（判定是否需要引导）。

---

## 7. adaptor：上游适配器

[crates/adaptor/src/traits.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/adaptor/src/traits.rs)

```rust
#[async_trait]
pub trait Adaptor: Send + Sync {
    fn provider(&self) -> ChannelProvider;
    // 非流式：发送并返回完整 (status, body)
    async fn execute(&self, base_url: &str, api_key: &str, body: Value, headers: HashMap<String,String>) -> ProxyResult<(u16, Bytes)>;
    // 流式：返回 reqwest::Response（已收到上游响应头，未读 body）
    async fn execute_stream(&self, base_url: &str, api_key: &str, body: Value, headers: HashMap<String,String>) -> ProxyResult<reqwest::Response>;
    // 从流式 chunk 提取 usage（每个 provider 位置不同）
    fn extract_usage(&self, chunk: &Bytes) -> Option<Usage>;
}
```

Adaptor 内部决定同格式（完整反序列化+适配）还是跨格式（走 Translator）—— 格式是否转换由 **入口格式 vs 渠道 provider** 决定，模型名只决定选哪个渠道。

### OpenaiAdaptor — [openai.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/adaptor/src/openai.rs)
- 端点：`{base_url}/chat/completions`
- 认证头：`Authorization: Bearer {api_key}`
- **流式时主动注入 `stream_options.include_usage=true`**，强制上游在最后 chunk 返回 usage
- `extract_usage`：解析 `data: {json}`，提取 `usage` 字段

### ClaudeAdaptor — [claude.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/adaptor/src/claude.rs)
- 端点：`{base_url}/v1/messages`
- 认证头：`x-api-key: {api_key}` + `anthropic-version: 2023-06-01`
- `extract_usage`：解析 `message_start`（取 input_tokens）和 `message_delta`（取 output_tokens）

两个 adaptor 都是无状态的，每次调用构造新实例。

---

## 8. translator：跨格式转换

[crates/translator/src/lib.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/translator/src/lib.rs) 导出四个非流式转换函数 + 流式状态机模块。

### 8.1 非流式转换

| 函数 | 文件 | 说明 |
|------|------|------|
| `openai_to_claude_request` | [o2c.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/translator/src/o2c.rs) | OpenAI 请求 → Claude 请求 |
| `claude_to_openai_response` | o2c.rs | Claude 响应 → OpenAI 响应 |
| `claude_to_openai_request` | [c2o.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/translator/src/c2o.rs) | Claude 请求 → OpenAI 请求 |
| `openai_to_claude_response` | c2o.rs | OpenAI 响应 → Claude 响应 |

**O2C 请求转换关键点**：
- `messages[].role=system` → 合并到 `system` 字段（数组形式）
- 同 role 连续消息合并（Claude 要求 user/assistant 严格交替）
- 首消息非 user 时补占位 `{"role":"user","content":"."}`
- `content` string → `[{type:"text", text:"..."}]`
- `tool_calls` → `tool_use` content block；`role=tool` → `tool_result`（role 变 user）
- `tools[].function` → `tools[].input_schema`
- `parallel_tool_calls` → `disable_parallel_tool_use`（布尔反转）
- `response_format` → 追加 system prompt 指令
- `reasoning_effort` → `thinking: {type:"enabled", budget_tokens: N}`
- `max_tokens` 缺失默认 4096
- 丢弃 `stream`/`n`/`presence_penalty` 等不支持字段

**C2O 请求转换**为上述逆操作；`thinking.budget_tokens` → `reasoning_effort`（<20000→low, <40000→medium, else→high）。

### 8.2 流式状态机 — [stream_state.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/translator/src/stream_state.rs)

跨格式流式转换不是逐 chunk 映射，而是维护完整状态机处理时序错位（借鉴 new-api `relay_stream.go`）。

#### `OpenaiToClaudeStreamState`（OpenAI SSE → Claude SSE）
状态字段：`content_block_index` / `tool_call_index` / `started` / `deferred_usage` / `current_tool_id` / `finished` / `message_stopped`。
- `process_chunk(chunk: &Bytes) -> Vec<Bytes>`：处理一个原始 SSE chunk（可能含多行 `data:`），返回若干 Claude 事件字节。
- 转换映射：
  - 首个 chunk → `message_start` + `content_block_start`(text, index 0)
  - `delta.content` → `content_block_delta`(text_delta)
  - `delta.tool_calls` → 维护 index，`content_block_start`(tool_use) + `input_json_delta`
  - `finish_reason` → `content_block_stop` + `message_delta`(含 usage, stop_reason 映射) + `message_stop`
  - usage 时序：缓冲到 `deferred_usage`，在 `message_delta` 时发送
  - `[DONE]` 不再产生输出（message_stop 已在 finish_reason 发出）

#### `ClaudeToOpenaiStreamState`（Claude SSE → OpenAI SSE）
状态字段：`started` / `current_block_type` / `current_block_index` / `tool_call_index` / `deferred_usage` / `input_tokens`。
- `process_event(event: &str, data: &Bytes) -> Vec<Bytes>`：处理一个 Claude 事件，返回若干 OpenAI chunk 字节。
- 转换映射：
  - `message_start` → 缓冲 input_tokens，不立即输出
  - `content_block_start`(text/tool_use) → 首次时发 role=assistant chunk；tool_use 发 tool_calls 起始 chunk
  - `content_block_delta`(text_delta) → `delta.content` chunk
  - `content_block_delta`(input_json_delta) → `delta.tool_calls[].function.arguments` chunk
  - `content_block_stop` → 仅状态跟踪
  - `message_delta` → 缓冲 stop_reason + output_tokens
  - `message_stop` → finish_reason chunk + usage chunk + `data: [DONE]`
  - `ping` → 忽略

---

## 9. core：路由 / 健康 / 计费 / 执行

[crates/core/src/lib.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/lib.rs) 导出 7 个模块。

### 9.1 Normalizer — [normalizer.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/normalizer.rs)
模型名归一化器，`Arc<RwLock<HashMap<String, (i64, String)>>>`。
- `resolve(name) -> Option<(model_id, canonical_name)>` — **大小写不敏感**（key 与 value 都 lowercase），未知返回 None。
- `reload(mapping)` — 整体替换映射（write-once-per-reload，由 ConfigCache 调用）。

### 9.2 Router — [router.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/router.rs)
渠道+Key 双层路由器，纯函数式（无状态）。

```rust
pub struct RoutedKey { pub channel: ChannelConfig, pub key: KeyConfig, pub upstream_model_name: String }

pub fn route(channels: Vec<(ChannelConfig, Vec<KeyConfig>, String)>, user_group: &str, is_key_available: impl Fn(i64)->bool) -> Vec<RoutedKey>
```

**过滤**：
- `user_group`：仅保留 `channel.group`（逗号分隔）包含该 group 的渠道
- `is_key_available(key_id)`：false 的 Key 丢弃，无存活 Key 的渠道丢弃

**排序**（全局确定性，非加权随机）：
1. `cost_tier`：Free 优先于 Paid
2. `channel.priority` 升序（渠道商整体优先级，粗排）
3. `key_priority` 升序（同渠道内 Key 细排）
4. 免费额度剩余比例降序（白嫖最大化：剩余多的先用）
5. `price_per_1k_tokens` 升序（同优先级付费 Key 比价）

### 9.3 HealthManager — [health.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/health.rs)
Key 运行时健康状态管理，`Arc<RwLock<HashMap<i64, KeyRuntimeState>>>` + 可选 `SharedDb` 持久化。

Cooldown 粒度为 **per-(key_id, upstream_model_name)**：同一 Key 在一个上游模型上超时/限流，不影响它服务其他上游模型。`status` 仅记录持久状态（Active/Disabled），瞬态 cooldown 存于 `cooldowns` map。

```rust
pub struct CooldownEntry {
    pub cooldown_until: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}

pub struct KeyRuntimeState {
    pub key_id: i64,
    pub status: KeyStatus,                              // Active / Disabled
    pub cooldowns: HashMap<String, CooldownEntry>,      // key = upstream_model_name
    pub used_quota_this_period: u64,
}
```

关键方法：
- `is_available(key_id, upstream_model)` / `try_is_available(key_id, upstream_model)`（同步版，锁争用时乐观返回 true）
- `mark_cooldown(key_id, upstream_model)` — 指数退避 `2^n` 秒，上限 1800s（30min）；**仅在内存**，不实时写 DB；**不改 key.status**（保持 Active，仅冷却该 (key, model) 组合）
- `mark_disabled(key_id)` — 内存 + 立即写 DB（重启后保持 disabled）；清空所有 per-model cooldown
- `check_recoveries()` — 清理过期的 per-model cooldown 条目（内存回收 + 重置退避计数器）；**不写 DB**（cooldown 从不持久化）
- `restore_disabled(key_ids)` — 手动恢复 disabled Key
- `add_usage(key_id, tokens)` — 累加内存计数（路由排序用）
- `sync_key_from_db(key_id, status)` — 管理 API 改状态后同步内存
- `load_disabled_from_db()` — 启动时从 DB 恢复 disabled 状态（cooldown 状态丢弃，重启视为冷却结束）

### 9.4 BillingManager — [billing.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/billing.rs)
双层扣费：预扣 → 结算/退款。通过 `BillingRepo` trait 解耦存储层。

```rust
pub trait BillingRepo: Send + Sync {
    async fn get_user_quota(&self, user_id) -> Option<i64>;       // quota - used_quota
    async fn update_user_used_quota(&self, user_id, delta);
    async fn get_token_remain_quota(&self, token_id) -> Option<i64>;
    async fn update_token_remain_quota(&self, token_id, delta);
    async fn update_token_used_quota(&self, token_id, delta);
    async fn update_token_status(&self, token_id, status);
    async fn get_token_unlimited(&self, token_id) -> Option<bool>;
}

pub struct BillingSession { pub user_id, pub token_id, pub pre_charged, pub settled, pub token_unlimited }
```

- `pre_charge(repo, user_id, token_id, estimated)` — 校验用户额度 + Token 额度（unlimited 跳过 Token 层），不足返回错误无副作用，通过则同时扣两层。
- `settle(repo, &mut session, actual_cost)` — 多退少补；Token 层归零则标记 status=3（exhausted）。
- `refund(repo, session)` — 全额退还（请求失败时）。

### 9.5 ConfigCache — [cache.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/cache.rs)
进程内配置缓存，`Arc<RwLock<Option<CacheData>>>` + 持有 `Normalizer`。

```rust
pub struct CacheData {
    pub channels: Vec<ChannelConfig>,
    pub keys: HashMap<i64, Vec<KeyConfig>>,                       // channel_id → keys
    pub bindings: HashMap<i64, Vec<(i64, String)>>,               // model_id → (channel_id, upstream_name)
    pub model_pricing: HashMap<i64, ModelPricing>,
}

pub trait CacheLoader: Send + Sync {
    async fn load_all(&self) -> ProxyResult<CacheData>;
    async fn load_alias_mapping(&self) -> ProxyResult<HashMap<String, (i64, String)>>;
}
```

- `get(loader)` — lazy 加载（首次访问），同时刷新 Normalizer；并发加载 last-writer-wins。
- `invalidate()` — 丢弃快照，下次 `get` 重载（管理 API 写操作后调用）。
- `get_for_model(model_id, user_group, loader)` — 返回该模型绑定的 `(channel, keys, upstream_name)` 列表（group 过滤交给 Router）。
- `get_model_pricing(model_id, loader)`。

**两层状态分离**：配置缓存（channel_configs，Web 面板改 → invalidate）与运行时状态（key_runtime_states，请求处理改 → 直接改内存，不走 invalidate）。

### 9.6 Tracker — [tracker.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/tracker.rs)
通过 `UsageWriter` trait 写用量。

```rust
pub trait UsageWriter: Send + Sync {
    async fn log_usage(&self, user_id, token_id, channel_id, key_id, model_id, &Usage, quota_cost, request_type, status, error: Option<&str>);
    async fn add_key_usage(&self, key_id, tokens);
}
```

- `track_success(...)` — 写 usage_logs（status=success）+ 累加 DB 的 key.used_quota + 累加内存 health 计数。
- `track_failure(...)` — 写 usage_logs（status=failed, usage=0），**不动配额计数器**（未消耗 token）。

### 9.7 Executor — [executor.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/core/src/executor.rs)
请求执行器，串联上述所有组件。

```rust
pub enum EntryFormat { OpenAI, Claude }   // 客户端入口格式

pub struct ExecutionContext {
    pub user_id: i64, pub token_id: i64, pub user_group: String,
    pub model_id: i64, pub canonical_name: String,
}

pub struct Executor { pub health: Arc<HealthManager>, pub cache: Arc<ConfigCache> }
```

**关键函数**：

- `select_keys(ctx, loader)` — `cache.get_for_model()` → `Router::route()`（传入 per-(key, model) 可用性闭包），返回排序后的候选 Key。**不调 `check_recoveries`**（后台 10s 跑一次；`is_available` 内联检查 cooldown_until 实现零延迟恢复）。
- `classify_failure(e) -> FailureAction` — 三档分类：invalid_request→`ReturnToClient`，fatal(401/403)→`Disable`，其他→`Cooldown`。
- `estimate_cost(key, model_pricing)` — 预扣估算（按 1000 token 估算，优先 key 级价格，其次 model 定价，无定价则 0=免费）。
- `actual_cost(usage, key, model_pricing)` — 实际成本计算。
- `prepare_request(entry_format, body, channel, upstream_name)` — 同格式仅替换 model；跨格式调 Translator 转换再替换 model。返回 `(body, adaptor_provider)`。
- `translate_response_back(entry_format, adaptor_provider, body)` — 响应转回入口格式。

**非流式 `execute(...)`** 流程：
1. `select_keys` 取候选；空则返回 `AllKeysExhausted`。
2. `BillingManager::pre_charge`（按第一个候选估算）。
3. 遍历候选：`is_available(key_id, upstream_model)` → `prepare_request` → `adaptor.execute` → 成功则提取 usage、`settle`、`track_success`、（跨格式则 `translate_response_back`）返回；失败按 `classify_failure` 处理（Disable/Cooldown 继续，ReturnToClient 退款返回；Cooldown 调 `mark_cooldown(key_id, upstream_model)` 只冷却该组合）。
4. 全部失败：退款 + `AllKeysExhausted`。

**流式 `execute_stream(...)`** 流程：与非流式相同到 adaptor 调用，返回 `StreamBootstrap { response, session, routed_key, model_pricing, entry_format, adaptor_provider }`。**bootstrap 边界**：adaptor 返回 `Ok(resp)` 即已连上上游、不可再切 Key；后续逐 chunk 转发、usage 提取、billing settle、track 由 HTTP handler（`stream_sse_response`）完成。

---

## 10. server：HTTP 服务层

### 10.1 入口 — [main.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/main.rs)

启动流程：
1. 解析 CLI 第一个参数为 config 路径（默认 `config.yaml`）。
2. `load_config` + `init_tracing`。
3. `open_db` + `run_migrations`（校验 schema 版本，不匹配则报错）。
4. `ensure_default_admin`（users 表空时创建 `admin/admin123`，role=100）。
5. `bootstrap::is_db_empty` 时从配置的 `bootstrap.config_file` 导入。
6. 构建 `AppState`（executor/cache/health/normalizer/storage/db/config/session_store/active_streams）。
7. 启动两个后台任务：每 30s `health.check_recoveries()`；daily/monthly quota 重置。
8. `build_router` → `axum::serve` + 优雅关闭（Ctrl+C，等待 in-flight streaming billing 任务最多 30s）。

### 10.2 配置 — [config.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/config.rs)

```rust
pub struct AppConfig {
    pub server: ServerConfig,   // host/port/tls
    pub log: LogConfig,         // level
    pub bootstrap: BootstrapConfig, // config_file
    pub database: DatabaseConfig,   // path (默认 chennix.db)
}
pub fn load_config(path: &str) -> ProxyResult<AppConfig>;
pub fn ensure_default_admin(conn: &Connection) -> ProxyResult<()>; // 首次创建 admin/admin123
```

### 10.3 共享状态 — [state.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/state.rs)
```rust
pub type SharedDb = Arc<Mutex<Connection>>;   // tokio Mutex（rusqlite !Sync）
pub struct AppState {                          // derives Clone
    pub executor, cache, health, normalizer, storage: Arc<...>,
    pub db: SharedDb, pub config: Arc<AppConfig>,
    pub session_store: SessionStore,           // 管理面板 session
    pub active_streams: Arc<AtomicUsize>,      // 在途流式任务计数
}
```

### 10.4 存储适配 — [pipeline.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/pipeline.rs)
`StorageAdapter` 桥接 rusqlite repos 与 core 层，**同时实现三个 trait**：`CacheLoader`、`BillingRepo`、`UsageWriter`，所有方法通过 `db.lock().await` 串行访问 SQLite。

### 10.5 中间件

- [middleware/auth.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/middleware/auth.rs)：
  - `token_auth_middleware` — 提取 `Authorization: Bearer`，判定客户端 IP（`x-forwarded-for` → `x-real-ip`），`TokenRepo::validate_token` 校验，注入 `AuthContext` 到 extensions；失败 401。
  - `require_role(min_role)` — RBAC Layer 工厂，读 `AdminAuthContext`（管理 session）或 `AuthContext`（代理 token），role 不足 403。角色等级：Guest(0)/Common(1)/Admin(10)/Root(100)。
  - `ApiError(ProxyError)` — newtype，`IntoResponse` 实现，代理路由 handler 的错误类型。
- [middleware/logging.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/middleware/logging.rs)：`request_log_layer()` 返回 `tower_http::TraceLayer`，记录 method/path/status/latency。

### 10.6 代理路由 — [routes/](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/routes)

[routes/mod.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/routes/mod.rs) 定义共享管道：

```rust
pub async fn proxy_request(state, auth, entry_format: EntryFormat, body: Value) -> Result<Response, ProxyError>
```
流程：提取 model → `normalizer.resolve`（lazy 加载 cache）→ 校验 token model_limits → 构建 `ExecutionContext` → 按 `stream` 分支：非流式 `executor.execute` → JSON；流式 `executor.execute_stream` → `stream_sse_response`。

`stream_sse_response`：`tokio::spawn` 后台任务，逐 chunk 读上游 → adaptor.extract_usage 累加 → 跨格式则用流式状态机转换 → 通过 mpsc channel 转发客户端 → 流结束 `settle` + `track_success`（或失败 `refund`）。`ActiveStreamGuard` RAII 守卫保证 `active_streams` 计数正确。

- [openai.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/routes/openai.rs)：`POST /v1/chat/completions` → `proxy_request(EntryFormat::OpenAI)`。
- [claude.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/routes/claude.rs)：`POST /v1/messages` → `proxy_request(EntryFormat::Claude)`。
- [models.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/routes/models.rs)：`GET /v1/models`，从 DB 读所有模型，按 token.model_limits 过滤（大小写不敏感），返回 OpenAI 格式列表。

### 10.7 管理 API — [admin/](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/admin)

- [admin/auth.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/admin/auth.rs)：session cookie 认证。`SessionStore = Arc<RwLock<HashMap<String, (i64, i64)>>>`（session_token → user_id, created_at），24h TTL 懒清理。
  - `login_handler` — bcrypt 校验密码 → 建 session → `Set-Cookie: chennix_session=...; HttpOnly; SameSite=Strict; Max-Age=86400`（TLS 时加 Secure）。
  - `logout_handler` / `me_handler`。
  - `session_middleware` — 读 cookie → 查 session → 加载 user → 注入 `AdminAuthContext`。
- [admin/error.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/admin/error.rs)：`AdminError`（NotFound/BadRequest/Unauthorized/Forbidden/Conflict/BadGateway/Internal），`IntoResponse` 输出 `{"error","code"}`。
- [admin/handlers.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/admin/handlers.rs)：全部 CRUD/仪表盘/用量/日志/reload/连接测试/模型发现 handler。普通用户数据隔离（自动加 `user_id` 过滤），管理员可见全部。
- [admin/routes.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/admin/routes.rs)：`admin_router(state)` 组装三组路由：
  - **公开**：`POST /admin/api/auth/login`
  - **用户级**（session_middleware）：`/auth/logout`、`/auth/me`、`/dashboard`、`/me/password`、`/tokens` CRUD、`/tokens/:id/usage`、`/usage`、`/logs`
  - **管理员级**（session_middleware + `require_role(10)`）：`/users` CRUD、`/channels` CRUD + `/test`、`/channels/:id/keys` CRUD + `/reset-quota`、`/channels/:id/models` + `/discover-models`、`/discover-models`（表单版）、`/models` CRUD + `/pricing` + `/test` + `/aliases` + `/bindings`、`/reload`
  
  **层序**：`require_role(10)` 在内层，`session_middleware` 在外层，确保 session 先注入 `AdminAuthContext`。

### 10.8 静态文件 — [static_files.rs](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/src/static_files.rs)
`rust-embed` 编译期嵌入 `static/` 目录（debug 从磁盘读）。`web_routes()` 提供：
- `GET /admin` / `/admin/` → `index.html`
- `GET /admin/*path` → 嵌入文件，`api/` 开头返回 404，否则 SPA 回退 `index.html`
- `GET /assets/*path` → 直接服务（无 SPA 回退）
- `GET /favicon.svg`

`/admin/api/*` 路由更具体，axum 优先匹配；不存在的 `/admin/api/*` 返回 404 而非 SPA 回退。

---

## 11. Web 管理面板（前端）

位于 [crates/server/web/](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/web/)，构建产物输出到 `crates/server/static/`（被后端嵌入）。

### 技术栈
React 19 + TypeScript + Vite 6 + Tailwind 4（`@tailwindcss/vite`）+ shadcn/ui 风格组件（Radix UI 原语）+ zustand（auth 状态）+ axios（HTTP）+ recharts（图表）+ lucide-react（图标）+ react-router-dom v7。**未用 TanStack Query**，数据获取靠 `useEffect`+`useState`+`useCallback` 手动管理。

### 关键文件
- [vite.config.ts](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/web/vite.config.ts) — 端口 5173，dev 代理 `/admin/api` 与 `/v1` 到 `localhost:8080`；`build.outDir: '../static'`。
- [src/lib/api.ts](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/web/src/lib/api.ts) — 共享 axios 实例，`baseURL: '/admin/api'`，`withCredentials: true`（cookie 认证）。401 拦截器重定向到 `/admin/login`。`src/lib/api/` 下分模块（channels/dashboard/logs/models/tokens/usage/users）。
- [src/stores/auth.ts](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/web/src/stores/auth.ts) — Zustand store：`{ user, loading, setUser, logout }`，无持久化（每次加载通过 `/auth/me` 重建）。
- [src/App.tsx](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/web/src/App.tsx) — 启动调 `authApi.me()`，`BrowserRouter` 路由：`/admin/login` 公开；`/admin` 下 `ProtectedLayout`（auth guard）嵌套 dashboard/channels/models/users/tokens/usage/logs。

### 页面（[src/pages/](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/web/src/pages/)）
| 页面 | 职责 |
|------|------|
| Login | 用户名/密码登录 |
| Dashboard | 概览卡片（今日 token/请求/错误/可用 Key）+ Top 模型柱状图 + Token 配额饼图 + 最近请求表 |
| Channels | 渠道 CRUD + Key 管理（复制/掩码/状态徽章）+ 模型发现/绑定 + 连接测试 + 缓存 reload |
| Models | 标准模型 + 别名 + 绑定 + 定价管理 |
| Users | 用户管理（管理员） |
| Tokens | API Token CRUD（用户管理自己的） |
| Usage | 用量统计（用户看自己的，管理员看全部） |
| Logs | 请求日志分页查看 |

---

## 12. 请求处理流程详解

### 12.1 非流式请求

```
1. POST /v1/chat/completions  Body: {"model":"deepseek-chat",...}
2. token_auth_middleware: Bearer sk-chennix-xxx → validate_token → AuthContext{user,token}
3. proxy_request: 提取 model="deepseek-chat"
4. Normalizer.resolve("deepseek-chat") → (model_id=7, "deepseek-v3")  // 别名归一化
5. 校验 token.allows_model("deepseek-v3")
6. Executor.execute(ctx, EntryFormat::OpenAI, body):
   a. select_keys: cache.get_for_model(7) → Router::route
      → 排序后 [key2(free), key1(paid)]
   b. BillingManager.pre_charge(user, token, estimate)  // 双层扣
   c. 遍历候选:
      key2: is_available(key2, upstream_model) → prepare_request(同格式, 替换 model 为 upstream_name)
            → OpenaiAdaptor.execute(base_url, api_key, body) → Ok(bytes)
            → extract_usage → actual_cost → settle → track_success → 返回
      (失败 429: mark_cooldown(key2, upstream_model) → 试 key1)
7. 返回 JSON 给客户端
```

### 12.2 流式请求

```
1-5. 同上
6. Executor.execute_stream(...):
   a-b. 同非流式（select_keys + pre_charge）
   c. 遍历候选: adaptor.execute_stream → Ok(resp) → 返回 StreamBootstrap
      (bootstrap 阶段失败可切下一 Key；一旦 Ok(resp) 不可再切)
7. stream_sse_response(StreamBootstrap):
   tokio::spawn 后台任务:
   - 循环读上游 chunk
   - adaptor.extract_usage(chunk) 累加 usage
   - 跨格式: stream_state.process_chunk / process_event 转换
   - 通过 mpsc channel 转发客户端
   - 流结束: settle(session, actual_cost) + track_success
   - 失败: refund(session)
```

### 12.3 跨格式转换请求

```
客户端 POST /v1/chat/completions {"model":"claude-sonnet-4",...}  // OpenAI 入口
→ Normalizer → 选中 Anthropic 渠道
→ prepare_request(EntryFormat::OpenAI, ChannelProvider::Anthropic):
   跨格式 → chennix_translator::o2c::openai_to_claude_request(body) → 替换 model
→ ClaudeAdaptor.execute(发 /v1/messages) → 收到 Claude 响应
→ translate_response_back(OpenAI, Anthropic, body):
   o2c::claude_to_openai_response → OpenAI 格式
→ 返回客户端
（流式则用 OpenaiToClaudeStreamState / ClaudeToOpenaiStreamState 状态机转换）
```

---

## 13. 错误处理与冷却机制

### 错误三档分类

| 分类 | 触发 | 处理 |
|------|------|------|
| retryable | 429 / 5xx / 网络错误 | mark_cooldown 该 (key, upstream_model) 组合，试下一候选；**不影响同 Key 的其他模型** |
| fatal | 401 / 403（Key 无效） | mark_disabled 该 Key（内存+DB），继续试其他 Key |
| invalid_request | 400 / 422 / InvalidRequest | **立即返回客户端**，退款，不重试不标记 Key |
| 流式中途错误 | 已发 chunk 后出错 | 无法切渠道，发 SSE 错误事件终止流 |

### 冷却退避

- 指数退避：`2^n` 秒（n=该 (key, upstream_model) 组合的连续失败次数），上限 1800s（30min）。
- 冷却粒度为 **per-(key_id, upstream_model_name)**：同一 Key 在模型 A 上限流，仍可服务模型 B。
- 冷却状态**仅在内存**（`cooldowns` map），不实时写 DB；冷却超时自动恢复（`check_recoveries` 清理过期条目）。
- `disabled` 立即写 DB（重启保持）；启动时 `load_disabled_from_db` 恢复，cooldown 状态丢弃。
- 一个 Key 在某模型上限流不影响同渠道其他 Key，也不影响该 Key 服务其他模型；渠道下所有 Key 不可用 → 跳下一渠道。

### AllKeys 耗尽（503）

- `AllKeysDisabled` — 所有 Key 已 disabled（需去面板修复）
- `AllKeysCooldown` — 所有 Key 冷却中（返回最近恢复时间）
- `AllKeysQuotaExhausted` — 所有免费 Key 额度用完
- `AllKeysExhausted` — Key 用尽但状态混合（附尝试过的 Key 列表 + 最后错误）

---

## 14. 配置文件与运行方式

### 14.1 config.yaml（启动配置）

```yaml
server:
  host: "0.0.0.0"
  port: 8080
  tls: { enabled: false, cert: "", key: "" }   # TLS 未实现内置终止，建议反代
log:
  level: "info"   # trace|debug|info|warn|error，可被 RUST_LOG 覆盖
bootstrap:
  config_file: "bootstrap.yaml"   # 空字符串禁用引导
database:
  path: "chennix.db"
```

### 14.2 bootstrap.yaml（首次启动引导）

仅在 DB 为空（channels 表 0 行）时导入一次，结构对应 SQLite 三张表：`models`（+aliases）、`channels`、`keys`、`bindings`（model↔channel，含 upstream_model_name）。详见 [bootstrap.example.yaml](file:///c:/Users/chenniX/Desktop/chenniX-api/bootstrap.example.yaml)。后续修改通过管理 API/面板。

### 14.3 构建与运行

```bash
# 后端（workspace 根目录）
cargo build --release           # 产物 target/release/chennix-api
cargo run --release --bin chennix-api -- config.yaml

# 前端（crates/server/web/）
npm install
npm run build                   # 产物输出到 crates/server/static/
# 开发模式：npm run dev（端口 5173，代理 API 到 8080）

# 运行
./target/release/chennix-api config.yaml
# 默认管理员：admin / admin123（首次启动自动创建，请立即改密）
```

### 14.4 默认管理员
`ensure_default_admin` 在 users 表空时创建：username=`admin`，password=`admin123`（bcrypt），role=100（root），quota=999999999，group=`default`。

---

## 15. 测试策略

- **单元测试**：每个 crate 内嵌 `#[cfg(test)] mod tests`，覆盖：
  - common：状态判定、Usage 累加、UserConfig/TokenConfig 序列化与 helper
  - storage：`init_db` 幂等、`run_migrations` 版本校验、各表存在性与列
  - adaptor：`extract_usage` 解析 OpenAI/Claude chunk
  - translator：流式状态机简单文本流、工具调用、finish_reason 映射、usage 时序、多工具、ping 忽略
  - core：router 排序（free/priority/group/ratio/price）、health 冷却退避与恢复、billing 预扣/结算/退款/unlimited、cache invalidate/lazy 加载、executor 错误分类与 select_keys
- **集成测试**：[crates/server/tests/](file:///c:/Users/chenniX/Desktop/chenniX-api/crates/server/tests/)，用 `wiremock` mock 上游 API：
  - `scenarios_basic.rs` — 多用户同模型、双层扣费、group 路由、token model_limits
  - `scenarios_advanced.rs` — 跨格式转换、流式、错误重试
  - `tests/common/mod.rs` — 共享测试夹具（mock_openai/mock_claude、send_chat_request 等）
- **管理 API 测试**：admin/auth.rs 内用 `tower::ServiceExt::oneshot` 测 login/me/logout 与 RBAC 层序。

---

## 16. 关键设计决策与借鉴

| 决策 | 来源 | 应用方式 |
|------|------|---------|
| Adaptor 适配器模式 | new-api | 每个 provider 一个 Adaptor，内部决定同/跨格式 |
| 完整反序列化 + 适配 | new-api | 同格式也完整反序列化（注入 include_usage、字段过滤、model 替换） |
| 流式转换状态机 | new-api relay_stream.go | 维护 index/finish_reason/usage 时序状态，非逐 chunk 映射 |
| 错误三档分类 | CLIProxyAPI | retryable/fatal/invalid_request，400 类不重试不标记 |
| 主动注入 include_usage | CLIProxyAPI | 流式强制上游返回 usage |
| 无 usage 兜底 | CLIProxyAPI | 整流无 usage 仍记录请求（usage=0） |
| Key 级冷却 + 指数退避 | CLIProxyAPI | 自动恢复 |
| 运行时状态全内存 | CLIProxyAPI | 冷却/失败次数内存持有，异步持久化，不查 DB |
| 统一执行循环 | CLIProxyAPI conductor.go | `execute_with_retry`（Rust 简化版） |
| 流式 bootstrap retry | CLIProxyAPI | 首字节前可切 Key |
| Web 面板内嵌 | new-api | `rust-embed` 嵌入静态文件，单 binary 部署 |
| 渠道+Key 双层架构 | 自有 | 一个渠道挂多 Key，Key 级独立额度/冷却/健康 |
| 全局确定性排序 | 自有（不借鉴 new-api 加权随机） | 自用场景接受单 Key 独占到冷却的代价 |
| 双层扣费 | 自有 | 用户总额度 + Token 独立额度，预扣→结算 |
| 配置缓存与运行时状态分离 | 自有 | invalidate 只刷配置，不覆盖冷却状态；快照语义避免竞态 |

**不包含（后续迭代）**：用户注册/OAuth、Casbin 细粒度权限、支付充值、余额 API 自动查询、渠道权重负载均衡、插件系统、config.yaml 热加载、更多 provider（Gemini/Kimi/xAI）、请求缓存去重、模型合并自动建议、定时抓取脚本（设计文档提及但 MVP 未实现）。

# chenniX-api

轻量多用户 AI API 代理网关，将多个 AI 提供商的 API 统一为 OpenAI 兼容格式对外暴露。

## 解决的问题

1. **模型名归一化** — 同一模型在不同渠道名字不一（`glm5.1`、`zhipu/GLM5.1`），代理统一映射，客户端只用标准名
2. **多渠道优先级路由** — 同一模型挂多个渠道，用户通过管理面板拖拽设置渠道和 Key 的优先级，失败自动重试、Key 级别冷却
3. **跨格式转换** — 客户端用 OpenAI 格式可以调 Claude 后端，反之亦然（含流式状态机转换）

## 架构

```
Internet → Caddy (80/443, 自动 HTTPS) → chennix-api (8080) → 上游 AI API
```

```
客户端请求
  │
  ▼
Token 认证中间件（Bearer Token → 用户 + 额度）
  │
  ▼
模型名归一化（别名 → 标准名，大小写不敏感）
  │
  ▼
渠道 & Key 路由（按用户设置的优先级，失败自动重试）
  │
  ▼
计费预扣（双层：用户总额度 + Token 独立额度）
  │
  ▼
上游适配器（OpenAI / Claude，含流式转换）
  │
  ▼
结算 & 记录用量日志
```

## 快速开始

```bash
git clone https://github.com/<your-username>/chenniX-api.git
cd chenniX-api

cp config.example.yaml config.yaml
cp bootstrap.example.yaml bootstrap.yaml
cp .env.example .env

# 编辑 Caddyfile 把 example.com 改成你的域名
# 编辑 bootstrap.yaml 填入真实的 API Key

docker compose up -d
```

首次访问 `https://你的域名/admin` 会引导设置管理员密码。之后登录管理面板即可管理模型、渠道、Key、用户和 Token。

## 对外端点

| 端点 | 方法 | 说明 |
|------|------|------|
| `/v1/chat/completions` | POST | OpenAI 兼容，支持 stream |
| `/v1/messages` | POST | Claude 兼容，支持 stream |
| `/v1/models` | GET | 可用模型列表（按 Token 权限过滤） |
| `/admin/*` | GET | 管理面板 SPA |
| `/admin/api/*` | * | 管理面板后端 API |
| `/health` | GET | 健康检查 |

## 生产部署

> 1C2G 服务器不建议 `docker compose up -d --build`，Rust release 构建峰值需要 2–4 GB 内存。

推荐使用 CI 构建镜像 + 服务器拉取方式，详见 [DEPLOYMENT.md](DEPLOYMENT.md)。

```bash
# 拉取 GHCR 镜像
docker compose -f docker-compose.prod.yml pull
docker compose -f docker-compose.prod.yml up -d
```

## 配置

### 环境变量（`.env`）

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PORT` | 8080 | 监听端口 |
| `HOST` | 0.0.0.0 | 监听地址 |
| `DB_PATH` | /data/chennix.db | SQLite 路径 |
| `RUST_LOG` | info | 日志级别 |
| `CHENNIX_ADMIN_PASSWORD` | （空） | 设置则跳过 Web 向导自动建管理员 |

### bootstrap.yaml（首次启动导入）

首次启动时（channels 表为空），自动导入 models、channels、keys、bindings。之后修改请走管理面板。

### 数据持久化

| 挂载点 | 用途 |
|--------|------|
| `./data:/data` | SQLite 数据库 + WAL |
| `./config.yaml:/app/config.yaml:ro` | 配置文件 |
| `caddy_data` | Caddy HTTPS 证书 |

## 项目结构

```
chenniX-api/
├── crates/
│   ├── common/        # 共享类型与错误
│   ├── storage/       # SQLite 存储层（schema、CRUD、引导导入）
│   ├── adaptor/       # 上游适配器（OpenAI / Claude 实现）
│   ├── translator/    # OpenAI ↔ Claude 跨格式转换
│   ├── core/          # 核心逻辑（路由、计费、健康检查、执行循环）
│   └── server/        # HTTP 服务 + React 管理面板
│       ├── src/       # axum 路由、中间件、静态文件
│       └── web/       # React 19 + TypeScript + Vite 前端
├── docs/
│   └── CODE_WIKI.md   # 代码文档
├── .github/workflows/ # CI 构建 Docker 镜像并推送到 GHCR
├── Dockerfile
├── docker-compose.yml        # 本地构建部署
├── docker-compose.prod.yml   # 拉镜像部署（生产推荐）
├── DEPLOYMENT.md             # 部署指南
├── config.example.yaml
├── bootstrap.example.yaml
└── Cargo.toml                # Rust workspace
```

## 技术栈

| 组件 | 选型 |
|------|------|
| 语言 | Rust 2021 |
| HTTP 框架 | axum 0.7 |
| 异步运行时 | tokio |
| 数据库 | SQLite (rusqlite + bundled) |
| 前端 | React 19 + TypeScript + Vite 6 + Tailwind 4 + shadcn/ui |
| 反向代理 | Caddy（自动 HTTPS） |
| CI/CD | GitHub Actions → GHCR |

## 管理面板

React SPA，内嵌在二进制中（rust-embed），功能包括：

- 仪表盘（用量概览图表）
- 模型管理（CRUD + 渠道绑定）
- 渠道管理（CRUD + Key 管理 + 模型发现）
- 用户管理（CRUD + 额度管理）
- Token 管理（API Key 创建、额度、模型限制、IP 白名单）
- 请求日志（带过滤和分页）
- 连接测试

## 多用户 & 双层计费

- 管理员手动创建用户（无注册流程）
- 用户可创建多个 API Key（Token），每个 Token 配独立额度和模型限制
- **双层扣费**：用户总额度（银行账户）+ Token 独立额度（钱包）
- 请求时预扣 → 结算时多退少补
- 无限额 Token 跳过 Token 级检查，但仍扣用户总额度

## 数据库版本

项目不做向后兼容。代码版本与数据库 schema 版本不匹配时启动失败，需删库重建或用对应版本代码。详见 [DEPLOYMENT.md](DEPLOYMENT.md) 5.4 节。

## 本地开发

```bash
# 后端
cargo run -- config.yaml

# 前端（开发模式，热更新）
cd crates/server/web
npm install
npm run dev
```

前端开发服务器代理到 `localhost:8081`，与后端开发端口对齐。

## License

MIT
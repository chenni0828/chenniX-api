# 部署指南

chenniX-api 的部署与更新流程。覆盖 Docker Compose 生产部署、CI 镜像构建、低配服务器注意事项、占位符密钥告警、GitHub 拉取加速、数据库迁移机制。

---

## 一、前置条件

- 64 位 Linux 服务器（推荐 Ubuntu 22.04+ / Debian 12+）
- Docker 24+ 与 Docker Compose v2
- 已解析到服务器的域名（Caddy 自动申请 HTTPS 证书需要）
- 开放 80、443 端口（Caddy 入口）
- 至少 1 核 1G 内存（运行时需求；构建需求见下文）

---

## 二、推荐部署方式：CI 构建镜像 + 服务器拉取

> ⚠️ **不建议在生产服务器上直接 `docker compose up -d --build`**
>
> Rust release 构建期间峰值需要 2–4 GB 内存，1 核 2G 的轻量云服务器会触发 OOM Killed 或机器卡死。
> **正确做法**：在 CI（GitHub Actions / 自建 Runner）上构建并推送镜像，服务器端只执行 `docker pull` + `docker compose up -d`。

### CI 构建配置（已内置）

项目已内置 GitHub Actions workflow：[`.github/workflows/docker-publish.yml`](.github/workflows/docker-publish.yml)。

**镜像仓库选择 GHCR（GitHub Container Registry）**：无需额外注册 Docker Hub 账号，直接用 GitHub 账号，与仓库权限统一管理。

**首次启用步骤**：

1. **确认仓库权限**：仓库 `Settings → Actions → General → Workflow permissions` 设为 `Read and write`（默认已开启）
2. **推送代码触发构建**：push 到 `main` 分支或手动在 Actions 页面 `Run workflow`
3. **首次拉取镜像需登录**（GHCR 镜像默认是 private）：
   ```bash
   # 在服务器上用 GitHub PAT 登录
   echo "ghp_YOUR_PAT" | docker login ghcr.io -u YOUR_GITHUB_USERNAME --password-stdin
   ```
   PAT 需要 `read:packages` 权限。若想公开镜像，到仓库页面 `Packages → 对应包 → Package settings → Change visibility` 改为 public，则无需登录。

**workflow 特性**：
- 多架构支持：`linux/amd64` + `linux/arm64`（树莓派 / ARM 服务器友好）
- 标签策略：`latest`（默认分支）+ commit hash（便于回滚）+ 语义化版本（tag 时）
- GHA 缓存：二次构建只编译改动部分，从 15 分钟降到 3-5 分钟
- 触发条件：push 到 main（仅改动 src/Dockerfile/Cargo.toml 等关键文件时）+ 手动 + tag

### 服务器端部署（拉镜像方式）

使用专门的 [docker-compose.prod.yml](docker-compose.prod.yml)（不含 `build:` 块）：

```bash
mkdir chenniX-api && cd chenniX-api

# 1. 拉取部署文件（不需要完整仓库）
curl -L https://raw.githubusercontent.com/<owner>/<repo>/main/docker-compose.prod.yml -o docker-compose.yml
curl -L https://raw.githubusercontent.com/<owner>/<repo>/main/Caddyfile -o Caddyfile
curl -L https://raw.githubusercontent.com/<owner>/<repo>/main/.env.example -o .env
curl -L https://raw.githubusercontent.com/<owner>/<repo>/main/config.example.yaml -o config.yaml
curl -L https://raw.githubusercontent.com/<owner>/<repo>/main/bootstrap.example.yaml -o bootstrap.yaml

# 2. 编辑文件
#    - docker-compose.yml：把 <your-github-username> 替换为你的 GitHub 用户名（小写）
#    - Caddyfile：把 example.com 改成你的域名
#    - .env / config.yaml / bootstrap.yaml：按需配置

# 3. （私有镜像）登录 GHCR
echo "ghp_YOUR_PAT" | docker login ghcr.io -u YOUR_GITHUB_USERNAME --password-stdin

# 4. 拉取并启动
docker compose pull
docker compose up -d

# 5. 验证
docker compose logs -f chennix-api
curl http://localhost:8080/health
```

### 更新流程（CI 构建后）

```bash
cd chenniX-api

# 拉取最新镜像 + 重启容器（自动 recreate）
docker compose pull && docker compose up -d

# 验证
docker compose logs --tail=50 chennix-api
curl http://localhost:8080/health

# 回滚到指定版本（用 commit hash 标签）
docker tag ghcr.io/<owner>/<repo>:abc1234 ghcr.io/<owner>/<repo>:latest
docker compose up -d
```

> 💡 **数据库备份**：更新前务必备份 `data/chennix.db`，见 5.4 节。

---

## 三、备选方案：本地构建（仅推荐 ≥4G 内存的服务器）

如果服务器内存 ≥4G，可以直接本地构建：

```bash
git clone https://github.com/<owner>/<repo>.git chenniX-api
cd chenniX-api
cp .env.example .env
cp config.example.yaml config.yaml
cp bootstrap.example.yaml bootstrap.yaml
# 编辑 Caddyfile / .env / config.yaml / bootstrap.yaml
docker compose up -d --build
```

### 低配服务器（1C2G）必须构建时的缓解措施

```bash
# 临时添加 2G swap
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile

# 或限制构建内存（会更慢但不会 OOM）
DOCKER_BUILDKIT=1 docker build --memory=1.5g -t chennix-api .
```

---

## 四、配置说明

### 4.1 ⚠️ bootstrap.yaml 占位符密钥

`bootstrap.example.yaml` 中的 API Key 是占位符（`sk-your-openai-key-here` 等），**不能直接用于生产**。

启动时会自动扫描并输出醒目 WARNING：

```
┌─────────────────────────────────────────────────────────────┐
│  ⚠️  检测到 2 个疑似占位符 API Key：                          │
│    • [主账号] sk-your-o...here                                │
│    • [备用]   sk-ant-y...here                                 │
│                                                              │
│  这些 Key 大概率来自 bootstrap.example.yaml 的占位符。        │
│  请登录管理面板 → Channels → 修改为真实 API Key，             │
│  否则所有请求都会因 401 失败。                                │
└─────────────────────────────────────────────────────────────┘
```

看到此告警请登录管理面板 → Channels 修改为真实 Key。

### 4.2 环境变量优先级

`.env` 中的环境变量覆盖 `config.yaml`：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `PORT` | 8080 | 监听端口 |
| `HOST` | 0.0.0.0 | 监听地址 |
| `DB_PATH` | /data/chennix.db | SQLite 路径 |
| `UPSTREAM_TIMEOUT_SECS` | 60 | 非流式上游整体超时（秒） |
| `STREAMING_TIMEOUT_SECS` | 300 | 流式首字节到达超时（秒，不中断已建立的流） |
| `RUST_LOG` | info | 日志级别 |
| `CHENNIX_ADMIN_PASSWORD` | （空） | 设置则跳过 Web 向导自动建 admin |

### 4.3 首次访问初始化

- 若 `CHENNIX_ADMIN_PASSWORD` 已设且 users 表为空 → 启动时自动建管理员
- 否则首次访问 `https://你的域名/admin` 会重定向到 `/setup` 向导，引导设置管理员密码（≥8 位）

---

## 五、更新流程

### 5.1 标准更新（CI 镜像方式）

```bash
cd chenniX-api

# 1. 备份数据库（强烈建议）
cp data/chennix.db data/chennix.db.bak.$(date +%Y%m%d%H%M)
# WAL 模式下也要备份 -wal / -shm（如果存在）
cp data/chennix.db-wal data/chennix.db-wal.bak 2>/dev/null || true
cp data/chennix.db-shm data/chennix.db-shm.bak 2>/dev/null || true

# 2. 拉取新镜像并重启
docker compose pull
docker compose up -d

# 3. 验证
docker compose logs -f chennix-api   # 看启动日志
curl http://localhost:8080/health     # 健康检查
```

### 5.2 本地构建方式更新

```bash
cd chenniX-api

# 备份数据库（同上）
cp data/chennix.db data/chennix.db.bak.$(date +%Y%m%d%H%M)

# 拉取代码并重新构建
git pull origin main
docker compose up -d --build
```

### 5.3 GitHub 拉取加速（国内 / 韩国服务器）

`git clone` / `git pull` 在国内或韩国服务器上常因网络延迟超时。替代方案：

```bash
# 方式 A：tarball 直接下载（无需 git 历史）
curl -L https://github.com/<owner>/<repo>/tarball/main | tar xz --strip-components=1

# 方式 B：使用 GH_PROXY 加速
git clone https://ghproxy.com/https://github.com/<owner>/<repo>.git
# 或对已 clone 的仓库：
git remote set-url origin https://ghproxy.com/https://github.com/<owner>/<repo>.git
git pull

# 方式 C：配置 git 走代理（如果服务器有代理）
git config --global http.proxy http://127.0.0.1:7890
```

### 5.4 数据库版本校验

项目**不做向后兼容**。启动时 `run_migrations()` 校验数据库的 `schema_version` 是否匹配代码版本：

- **版本匹配**：直接通过，无任何操作
- **版本不匹配**：启动失败，报错信息示例：

  ```
  schema version mismatch: database is v0, code expects v1.
  This project does not support backward compatibility.
  Either use the matching code version or delete the database to reinitialize.
  ```

**全新库**：`init_db` 创建表时直接写入 `schema_version = CURRENT_SCHEMA_VERSION`，跳过校验。

**升级代码后遇到版本不匹配**：
1. 如果不需要保留数据：删除数据库重新初始化
   ```bash
   docker compose down
   rm data/chennix.db data/chennix.db-wal data/chennix.db-shm 2>/dev/null
   docker compose up -d
   ```
2. 如果需要保留数据：用旧版本代码启动，导出数据，再用新版本代码重新导入

---

## 六、故障排查

### 6.1 Rust 编译报错 `error[E0373]: closure may outlive the function`

**原因**：`main.rs` 的 graceful shutdown 块在 Rust 1.80+ 下，`async {}` 借用局部变量 `sigterm` 不满足 `'static` 约束。

**修复**：已改为 `async move`（[main.rs:224](crates/server/src/main.rs#L224)）。如果你 clone 的版本仍报此错，检查 main.rs 是否包含 `async move`，或升级到最新代码。

### 6.2 Docker 构建时 OOM Killed

```
=> ERROR [builder 4/4] cargo build --release    300s
=> # cc: fatal error: Killed signal terminated program cc1
```

**原因**：内存不足。

**解决**：见「低配服务器必须构建时的缓解措施」一节，或改用 CI 构建镜像方式。

### 6.3 启动后请求返回 401

**原因**：bootstrap.yaml 中的 API Key 是占位符。

**解决**：登录管理面板 → Channels → 编辑对应渠道 → 修改 API Key 为真实值。

### 6.4 启动后日志显示 `Detected placeholder API key`

这是占位符检测告警，见 4.1 节。按提示修改 Key 即可。

### 6.5 启动报错 `schema version mismatch`

项目不做向后兼容。代码版本与数据库版本不匹配时会报错：

```
schema version mismatch: database is v0, code expects v1.
```

**解决**：见 5.4 节「数据库版本校验」——删库重建或用对应版本代码。

---

## 七、数据持久化

| 挂载点 | 用途 | 必须持久化 |
|--------|------|-----------|
| `./data:/data` | SQLite 主库 + WAL | ✅ |
| `./config.yaml:/app/config.yaml:ro` | 配置文件 | ✅ |
| `caddy_data` | Caddy 证书 | ✅ |
| `caddy_config` | Caddy 配置 | 可选 |

`bootstrap.yaml` 仅首次导入使用（channels 表空时），之后修改不会生效，请走管理面板。

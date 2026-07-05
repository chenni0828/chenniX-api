# chennix-api 多阶段 Dockerfile
# 三阶段：node 前端构建 → rust 后端构建 → debian-slim 运行时
# 适配 1核1-2G VPS，镜像尽量精简

# ============================================================================
# Stage 1: 前端构建（node）
# ============================================================================
FROM node:20-bookworm-slim AS web-builder

WORKDIR /app/web

# 先复制 package*.json 利用 Docker 层缓存
COPY crates/server/web/package.json crates/server/web/package-lock.json* ./

# 安装依赖（ci 模式保证确定性构建）
RUN npm ci || npm install

# 复制前端源码并构建
COPY crates/server/web/ ./

# 构建输出到 ../static（即 crates/server/static/），被 rust-embed 嵌入
RUN npm run build

# ============================================================================
# Stage 2: 后端构建（rust）
# ============================================================================
FROM rust:1 AS backend-builder

WORKDIR /app

# 安装后端构建依赖
# - gcc: rusqlite bundled feature 从源码编译 SQLite，需要 C 编译器
# - pkg-config: 查找系统库
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# 先复制 Cargo.toml 利用 Docker 层缓存编译依赖
COPY Cargo.toml Cargo.lock* ./
COPY crates/ ./crates/

# 将前端构建产物复制到 rust-embed 读取的目录
# web-builder 的 WORKDIR 是 /app/web，vite 输出到 ../static → /app/static/
COPY --from=web-builder /app/static ./crates/server/static

# 诊断：确认前端产物存在（rust-embed 编译期需要 static/ 目录）
RUN ls -la crates/server/static/ | head -20

# Release 模式构建
RUN cargo build --release --bin chennix-api

# ============================================================================
# Stage 3: 运行时（debian-slim）
# ============================================================================
FROM debian:bookworm-slim AS runtime

# 安装运行时依赖：
# - ca-certificates: HTTPS 请求
# - tzdata: 时区支持
# - wget: 健康检查
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tzdata \
    wget \
    && rm -rf /var/lib/apt/lists/*

# 设置时区
ENV TZ=Asia/Shanghai

# 创建数据目录
RUN mkdir -p /data

WORKDIR /app

# 复制构建产物
COPY --from=backend-builder /app/target/release/chennix-api /app/chennix-api

# 复制默认配置（用户可通过 volume 挂载覆盖）
COPY config.example.yaml /app/config.yaml

# 暴露 HTTP 端口
EXPOSE 8080

# 健康检查（每 30s 检查 /health 端点）
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -q -O- http://localhost:8080/health || exit 1

# 直接执行二进制
ENTRYPOINT ["/app/chennix-api"]
CMD ["/app/config.yaml"]

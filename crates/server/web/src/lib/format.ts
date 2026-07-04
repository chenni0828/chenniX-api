/**
 * Shared formatting utilities for admin pages.
 */

/**
 * 将日期值格式化为北京时间字符串。
 *
 * 输入支持：
 * - Unix 时间戳（秒，number）
 * - SQLite datetime('now') 字符串（UTC，格式 "2026-07-04 17:51:00"）
 *
 * 输出统一为北京时间（Asia/Shanghai，UTC+8），格式 "2026-07-05 01:51:00"。
 * 显式指定时区，避免依赖浏览器/服务器本地时区设置。本项目面向中国用户，
 * 所有时间展示均以北京时间为准。
 *
 * 参考实现：new-api 使用 dayjs 配合时区插件；此处用原生 Intl.DateTimeFormat
 * 指定 timeZone: "Asia/Shanghai"，无需引入额外依赖。
 */
export function formatDate(ts: number | string | null | undefined): string {
  if (ts === null || ts === undefined) return "-"
  let d: Date
  if (typeof ts === "number") {
    if (ts <= 0) return "永不过期"
    d = new Date(ts * 1000)
  } else {
    if (!ts) return "-"
    // SQLite datetime('now') 存储的是 UTC 时间，格式 "2026-07-04 17:51:00"。
    // 末尾追加 'Z' 让 JS 按 UTC 解析；若不加，JS 会按本地时区解析导致偏移。
    d = new Date(ts.replace(" ", "T") + "Z")
  }
  if (isNaN(d.getTime())) return String(ts)
  // 显式指定 Asia/Shanghai 时区，确保无论运行环境时区如何都显示北京时间
  const fmt = new Intl.DateTimeFormat("zh-CN", {
    timeZone: "Asia/Shanghai",
    hour12: false,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  })
  // Intl 输出格式为 "2026/07/05 01:51:00"，统一替换为连字符以匹配 new-api 风格
  return fmt.format(d).replace(/\//g, "-")
}

/** Format a number with thousands separators. */
export function formatNumber(n: number | null | undefined): string {
  if (n === null || n === undefined) return "-"
  return n.toLocaleString("zh-CN")
}

/**
 * 内部配额单位常量：1 元 = 1,000,000 微元。
 *
 * 后端所有 money-quota 字段（users.quota / tokens.remain_quota /
 * usage_logs.quota_cost / request_logs.quota_cost）均以微元（i64）存储，
 * 保证整数运算无精度损失。前端展示与输入以「元」为单位，需通过此常量
 * 进行换算。参考 new-api 的 QuotaPerUnit 设计。
 */
export const QUOTA_PER_YUAN = 1_000_000

/**
 * 将微元（内部整数配额）格式化为元（人民币）字符串。
 *
 * - 入参为后端原始值（微元，i64 范围）
 * - 输出按人民币习惯展示：≤0.01 元时保留 6 位小数以体现微小成本，
 *   否则保留 2 位小数 + 千分位分隔
 *
 * 例：formatCost(1_500_000) → "1.50"
 *     formatCost(15)        → "0.000015"
 *     formatCost(0)         → "0"
 *     formatCost(null)      → "-"
 */
export function formatCost(n: number | null | undefined): string {
  if (n === null || n === undefined) return "-"
  const yuan = n / QUOTA_PER_YUAN
  if (!isFinite(yuan)) return "-"
  if (yuan === 0) return "0"
  // 微小成本（< 1 分）保留 6 位小数；否则保留 2 位
  const digits = Math.abs(yuan) < 0.01 ? 6 : 2
  return yuan.toLocaleString("zh-CN", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })
}

/**
 * 将元（人民币，浮点）转为微元（内部整数配额）。
 * 用于前端 quota 输入框提交到后端时的换算。
 *
 * 向上取整以避免扣费不足（与后端 actual_cost 的 .round() 取整一致）。
 * 例：yuanToQuota(0.000001) === 1
 */
export function yuanToQuota(yuan: number): number {
  return Math.round(yuan * QUOTA_PER_YUAN)
}

/**
 * 将微元（内部整数配额）转为元（人民币，浮点）。
 * 用于前端 quota 输入框回显后端值时的换算。
 */
export function quotaToYuan(quota: number): number {
  return quota / QUOTA_PER_YUAN
}

/** Compute quota usage percentage and display text (input in micro-yuan). */
export function formatQuota(used: number, total: number): { percent: number; text: string } {
  if (total <= 0) return { percent: 0, text: `${formatCost(used)} 元 / ∞` }
  const percent = Math.min(100, Math.round((used / total) * 100))
  return { percent, text: `${formatCost(used)} / ${formatCost(total)} 元` }
}

/** Mask a token key: sk-xxxx...xxxx */
export function maskKey(key: string): string {
  if (!key) return "-"
  if (key.length <= 12) return key
  return `${key.slice(0, 6)}...${key.slice(-4)}`
}

/** Extract error message from an axios error. */
export function getErrorMessage(err: unknown, fallback: string): string {
  const e = err as { response?: { data?: { message?: string; error?: string } }; message?: string }
  return e?.response?.data?.message || e?.response?.data?.error || e?.message || fallback
}

/** Generate a random API key with sk- prefix. */
export function generateKey(): string {
  const chars = "abcdefghijklmnopqrstuvwxyz0123456789"
  let key = "sk-"
  for (let i = 0; i < 32; i++) {
    key += chars[Math.floor(Math.random() * chars.length)]
  }
  return key
}

/**
 * 复制文本到剪贴板。
 *
 * 优先用 `navigator.clipboard.writeText`（安全上下文：HTTPS / localhost）；
 * 不可用时 fallback 到隐藏 textarea + `document.execCommand('copy')`，
 * 保证通过 IP 访问（非 HTTPS）时也能正常复制。
 *
 * 返回 true 表示成功，false 表示失败。
 */
export async function copyToClipboard(text: string): Promise<boolean> {
  // 1. 优先用现代 Clipboard API（安全上下文）
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // 权限被拒或其它错误 → 走 fallback
    }
  }
  // 2. fallback：隐藏 textarea + execCommand
  try {
    const ta = document.createElement("textarea")
    ta.value = text
    ta.style.position = "fixed"
    ta.style.top = "-9999px"
    ta.style.opacity = "0"
    document.body.appendChild(ta)
    ta.focus()
    ta.select()
    const ok = document.execCommand("copy")
    document.body.removeChild(ta)
    return ok
  } catch {
    return false
  }
}

/** Convert a datetime-local input value to Unix timestamp (seconds). */
export function datetimeLocalToTimestamp(value: string): number {
  if (!value) return -1
  const d = new Date(value)
  if (isNaN(d.getTime())) return -1
  return Math.floor(d.getTime() / 1000)
}

/** Convert a Unix timestamp (seconds) to a datetime-local input value. */
export function timestampToDatetimeLocal(ts: number): string {
  if (ts <= 0) return ""
  const d = new Date(ts * 1000)
  if (isNaN(d.getTime())) return ""
  const pad = (n: number) => String(n).padStart(2, "0")
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/** Get Unix timestamp for N days ago. */
export function daysAgoTimestamp(days: number): number {
  return Math.floor((Date.now() - days * 24 * 60 * 60 * 1000) / 1000)
}

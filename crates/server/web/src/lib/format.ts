/**
 * Shared formatting utilities for admin pages.
 */

/** Format a date value (Unix seconds or SQLite datetime string) to a readable string. */
export function formatDate(ts: number | string | null | undefined): string {
  if (ts === null || ts === undefined) return "-"
  if (typeof ts === "number") {
    if (ts <= 0) return "永不过期"
    return new Date(ts * 1000).toLocaleString("zh-CN")
  }
  if (!ts) return "-"
  // SQLite datetime format: "2026-07-01 12:34:56"
  const d = new Date(ts.replace(" ", "T"))
  if (isNaN(d.getTime())) return ts
  return d.toLocaleString("zh-CN")
}

/** Format a number with thousands separators. */
export function formatNumber(n: number | null | undefined): string {
  if (n === null || n === undefined) return "-"
  return n.toLocaleString("zh-CN")
}

/** Format a cost/quota value. */
export function formatCost(n: number | null | undefined): string {
  if (n === null || n === undefined) return "-"
  return n.toLocaleString("zh-CN")
}

/** Compute quota usage percentage and display text. */
export function formatQuota(used: number, total: number): { percent: number; text: string } {
  if (total <= 0) return { percent: 0, text: `${formatNumber(used)} / ∞` }
  const percent = Math.min(100, Math.round((used / total) * 100))
  return { percent, text: `${formatNumber(used)} / ${formatNumber(total)}` }
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

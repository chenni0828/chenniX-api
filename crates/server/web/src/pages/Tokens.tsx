import { useState, useEffect, useCallback } from "react"
import {
  Plus, Pencil, Trash2, Loader2, RefreshCw,
  Copy, Check, Infinity as InfinityIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import { Switch } from "@/components/ui/switch"
import { Card, CardContent } from "@/components/ui/card"
import {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from "@/components/ui/table"
import {
  Dialog, DialogContent, DialogDescription, DialogFooter,
  DialogHeader, DialogTitle,
} from "@/components/ui/dialog"
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select"
import { useAuthStore } from "@/stores/auth"
import { toast } from "@/hooks/use-toast"
import { tokenApi, type TokenConfig, type CreateTokenPayload, type UpdateTokenPayload, type TokenUsage } from "@/lib/api/tokens"
import { userApi, type UserConfig } from "@/lib/api/users"
import {
  formatNumber, formatDate, formatCost, maskKey, getErrorMessage, generateKey,
  datetimeLocalToTimestamp, timestampToDatetimeLocal, copyToClipboard,
  yuanToQuota, quotaToYuan,
} from "@/lib/format"

const STATUS_OPTIONS = [
  { value: "1", label: "启用" },
  { value: "2", label: "禁用" },
  { value: "3", label: "耗尽" },
]

function tokenStatusBadge(t: TokenConfig): { label: string; variant: "success" | "destructive" | "secondary" | "warning" } {
  const now = Math.floor(Date.now() / 1000)
  if (t.expired_time !== -1 && t.expired_time <= now) {
    return { label: "已过期", variant: "secondary" }
  }
  switch (t.status) {
    case 1: return { label: "正常", variant: "success" }
    case 2: return { label: "禁用", variant: "destructive" }
    case 3: return { label: "耗尽", variant: "warning" }
    default: return { label: "未知", variant: "secondary" }
  }
}

/** Quota usage progress bar */
function QuotaUsageCell({ token }: { token: TokenConfig }) {
  const [usage, setUsage] = useState<TokenUsage | null>(null)
  const [loading, setLoading] = useState(false)
  const [showDetail, setShowDetail] = useState(false)

  const totalQuota = token.used_quota + token.remain_quota
  const usagePercent = token.unlimited_quota
    ? 0
    : totalQuota > 0
      ? Math.min((token.used_quota / totalQuota) * 100, 100)
      : 0

  const handleMouseEnter = async () => {
    setShowDetail(true)
    if (!usage && !loading) {
      setLoading(true)
      try {
        const data = await tokenApi.getUsage(token.id)
        setUsage(data)
      } catch {
        // ignore — usage detail is optional
      } finally {
        setLoading(false)
      }
    }
  }

  if (token.unlimited_quota) {
    return (
      <span className="flex items-center gap-1 text-muted-foreground">
        <InfinityIcon className="h-3 w-3" />无限
      </span>
    )
  }

  return (
    <div
      className="relative group min-w-[120px]"
      onMouseEnter={handleMouseEnter}
      onMouseLeave={() => setShowDetail(false)}
    >
      <div className="flex items-center gap-2">
        <div className="flex-1 h-2 rounded-full bg-muted overflow-hidden">
          <div
            className={`h-full rounded-full transition-all ${
              usagePercent > 80 ? "bg-destructive" : usagePercent > 50 ? "bg-amber-500" : "bg-emerald-500"
            }`}
            style={{ width: `${usagePercent}%` }}
          />
        </div>
        <span className="text-xs tabular-nums text-muted-foreground whitespace-nowrap">
          {formatCost(token.used_quota)}/{formatCost(totalQuota)} 元
        </span>
      </div>
      {/* Hover detail popover */}
      {showDetail && (
        <div className="absolute z-50 top-full left-0 mt-1 w-56 rounded-md border bg-popover p-3 text-xs shadow-md">
          {loading ? (
            <div className="flex items-center gap-2 text-muted-foreground">
              <Loader2 className="h-3 w-3 animate-spin" />加载中...
            </div>
          ) : usage ? (
            <div className="space-y-1.5">
              <div className="flex justify-between">
                <span className="text-muted-foreground">总消耗 Token</span>
                <span className="font-medium tabular-nums">{formatNumber(usage.total_tokens)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">请求次数</span>
                <span className="font-medium tabular-nums">{formatNumber(usage.request_count)}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">最后使用</span>
                <span className="font-medium">{usage.last_used_at > 0 ? formatDate(usage.last_used_at) : "从未"}</span>
              </div>
            </div>
          ) : (
            <span className="text-muted-foreground">无法加载统计</span>
          )}
        </div>
      )}
    </div>
  )
}

export default function Tokens() {
  const currentUser = useAuthStore((s) => s.user)
  const isAdmin = currentUser ? currentUser.role >= 10 : false

  const [tokens, setTokens] = useState<TokenConfig[]>([])
  const [users, setUsers] = useState<UserConfig[]>([])
  const [filterUserId, setFilterUserId] = useState<string>("all")
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")

  const [createOpen, setCreateOpen] = useState(false)
  const [editTarget, setEditTarget] = useState<TokenConfig | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<TokenConfig | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [copiedId, setCopiedId] = useState<number | null>(null)

  const fetchTokens = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const userId = filterUserId === "all" ? undefined : parseInt(filterUserId)
      const data = await tokenApi.list(userId)
      setTokens(data)
    } catch (err) {
      setError(getErrorMessage(err, "加载令牌列表失败"))
    } finally {
      setLoading(false)
    }
  }, [filterUserId])

  const fetchUsers = useCallback(async () => {
    if (!isAdmin) return
    try {
      const data = await userApi.list()
      setUsers(data)
    } catch {
      // ignore — filter is optional
    }
  }, [isAdmin])

  useEffect(() => {
    fetchTokens()
  }, [fetchTokens])

  useEffect(() => {
    fetchUsers()
  }, [fetchUsers])

  // ── Non-admin: only show own tokens ──
  useEffect(() => {
    if (currentUser && !isAdmin) {
      setFilterUserId(String(currentUser.id))
    }
  }, [currentUser, isAdmin])

  const copyKey = async (t: TokenConfig) => {
    const ok = await copyToClipboard(t.key)
    if (ok) {
      setCopiedId(t.id)
      toast({ title: "Key 已复制到剪贴板" })
      setTimeout(() => setCopiedId(null), 2000)
    } else {
      toast({ title: "复制失败", variant: "destructive" })
    }
  }

  const getUsername = (uid: number) => {
    const u = users.find((x) => x.id === uid)
    return u ? u.username : `用户 ${uid}`
  }

  // ── Create ──
  // remain_quota 字段：输入以「元」为单位，提交时由 yuanToQuota 转为微元
  const [createForm, setCreateForm] = useState({
    name: "", assign_user_id: "", remain_quota: "10", unlimited_quota: false,
    expired_time: "", model_limits: "", model_limits_enabled: false,
    allow_ips: "",
  })

  const handleCreate = async () => {
    if (!createForm.name) {
      toast({ title: "请输入令牌名称", variant: "destructive" })
      return
    }
    setSubmitting(true)
    try {
      const payload: CreateTokenPayload = {
        name: createForm.name,
        key: generateKey(),
        remain_quota: yuanToQuota(parseFloat(createForm.remain_quota) || 0),
        unlimited_quota: createForm.unlimited_quota,
        expired_time: createForm.expired_time ? datetimeLocalToTimestamp(createForm.expired_time) : -1,
        model_limits: createForm.model_limits,
        model_limits_enabled: createForm.model_limits_enabled,
        allow_ips: createForm.allow_ips,
      }
      // Admin can optionally assign to another user via query parameter
      const assignUserId = isAdmin && createForm.assign_user_id
        ? parseInt(createForm.assign_user_id) || undefined
        : undefined
      await tokenApi.create(payload, assignUserId)
      toast({ title: "令牌创建成功" })
      setCreateOpen(false)
      setCreateForm({
        name: "", assign_user_id: "", remain_quota: "10", unlimited_quota: false,
        expired_time: "", model_limits: "", model_limits_enabled: false, allow_ips: "",
      })
      fetchTokens()
    } catch (err) {
      toast({ title: "创建失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setSubmitting(false)
    }
  }

  // ── Update ──
  const [editForm, setEditForm] = useState({
    name: "", remain_quota: "0", unlimited_quota: false,
    expired_time: "", model_limits: "", model_limits_enabled: false,
    allow_ips: "", status: "1",
  })

  const openEdit = (t: TokenConfig) => {
    setEditForm({
      name: t.name || "",
      // 后端存微元，回显时除以 QUOTA_PER_YUAN 转为元
      remain_quota: String(quotaToYuan(t.remain_quota)),
      unlimited_quota: t.unlimited_quota,
      expired_time: timestampToDatetimeLocal(t.expired_time),
      model_limits: t.model_limits?.join(", ") || "",
      model_limits_enabled: t.model_limits_enabled,
      allow_ips: t.allow_ips?.join(", ") || "",
      status: String(t.status),
    })
    setEditTarget(t)
  }

  const handleUpdate = async () => {
    if (!editTarget) return
    setSubmitting(true)
    try {
      const payload: UpdateTokenPayload = {
        name: editForm.name,
        remain_quota: yuanToQuota(parseFloat(editForm.remain_quota) || 0),
        unlimited_quota: editForm.unlimited_quota,
        expired_time: editForm.expired_time ? datetimeLocalToTimestamp(editForm.expired_time) : -1,
        model_limits: editForm.model_limits,
        model_limits_enabled: editForm.model_limits_enabled,
        allow_ips: editForm.allow_ips,
        status: parseInt(editForm.status),
      }
      await tokenApi.update(editTarget.id, payload)
      toast({ title: "令牌更新成功" })
      setEditTarget(null)
      fetchTokens()
    } catch (err) {
      toast({ title: "更新失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setSubmitting(false)
    }
  }

  // ── Delete ──
  const handleDelete = async () => {
    if (!deleteTarget) return
    setSubmitting(true)
    try {
      await tokenApi.delete(deleteTarget.id)
      toast({ title: "令牌已删除" })
      setDeleteTarget(null)
      fetchTokens()
    } catch (err) {
      toast({ title: "删除失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">令牌管理</h1>
          <p className="mt-1 text-sm text-muted-foreground">管理 API 访问令牌</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={fetchTokens} disabled={loading}>
            <RefreshCw className={`h-4 w-4 ${loading ? "animate-spin" : ""}`} />
            刷新
          </Button>
          <Button size="sm" onClick={() => setCreateOpen(true)}>
            <Plus className="h-4 w-4" />
            新建令牌
          </Button>
        </div>
      </div>

      {/* Filter */}
      {isAdmin && (
        <div className="flex items-center gap-3">
          <Label className="text-sm text-muted-foreground">按用户筛选</Label>
          <Select value={filterUserId} onValueChange={setFilterUserId}>
            <SelectTrigger className="w-48"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部用户</SelectItem>
              {users.map((u) => (
                <SelectItem key={u.id} value={String(u.id)}>{u.username}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}

      {/* Error */}
      {error && (
        <div className="rounded-md bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</div>
      )}

      {/* Table */}
      <Card>
        <CardContent className="p-0">
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : tokens.length === 0 ? (
            <div className="flex items-center justify-center py-16 text-sm text-muted-foreground">
              暂无令牌数据
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="min-w-[120px]">名称</TableHead>
                  {isAdmin && <TableHead>用户</TableHead>}
                  <TableHead className="min-w-[140px]">Key</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead className="min-w-[180px]">消耗</TableHead>
                  <TableHead className="text-right">剩余额度</TableHead>
                  <TableHead className="min-w-[140px]">过期时间</TableHead>
                  <TableHead>模型限制</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {tokens.map((t) => {
                  const st = tokenStatusBadge(t)
                  return (
                    <TableRow key={t.id}>
                      <TableCell className="font-medium">{t.name || "-"}</TableCell>
                      {isAdmin && <TableCell>{getUsername(t.user_id)}</TableCell>}
                      <TableCell>
                        <button
                          className="flex items-center gap-1.5 font-mono text-xs hover:text-primary"
                          onClick={() => copyKey(t)}
                          title="点击复制完整 Key"
                        >
                          {maskKey(t.key)}
                          {copiedId === t.id ? (
                            <Check className="h-3 w-3 text-emerald-500" />
                          ) : (
                            <Copy className="h-3 w-3 text-muted-foreground" />
                          )}
                        </button>
                      </TableCell>
                      <TableCell><Badge variant={st.variant}>{st.label}</Badge></TableCell>
                      <TableCell>
                        <QuotaUsageCell token={t} />
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {t.unlimited_quota ? (
                          <span className="flex items-center justify-end gap-1 text-muted-foreground">
                            <InfinityIcon className="h-3 w-3" />无限
                          </span>
                        ) : `${formatCost(t.remain_quota)} 元`}
                      </TableCell>
                      <TableCell className="text-xs">{formatDate(t.expired_time)}</TableCell>
                      <TableCell className="text-xs">
                        {t.model_limits_enabled && t.model_limits?.length
                          ? <code className="rounded bg-muted px-1.5 py-0.5">{t.model_limits.length} 个模型</code>
                          : <span className="text-muted-foreground">-</span>}
                      </TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          <Button variant="ghost" size="icon" className="h-8 w-8" onClick={() => openEdit(t)}>
                            <Pencil className="h-4 w-4" />
                          </Button>
                          <Button
                            variant="ghost" size="icon" className="h-8 w-8 text-destructive hover:text-destructive"
                            onClick={() => setDeleteTarget(t)}
                          >
                            <Trash2 className="h-4 w-4" />
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      {/* Create Dialog */}
      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent className="max-h-[90vh] max-w-lg overflow-y-auto">
          <DialogHeader>
            <DialogTitle>新建令牌</DialogTitle>
            <DialogDescription>创建一个新的 API 访问令牌</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="ct-name">名称</Label>
              <Input id="ct-name" value={createForm.name}
                onChange={(e) => setCreateForm({ ...createForm, name: e.target.value })}
                placeholder="输入令牌名称" />
            </div>
            {isAdmin && (
              <div className="space-y-2">
                <Label>指定用户（可选）</Label>
                <Select value={createForm.assign_user_id} onValueChange={(v) => setCreateForm({ ...createForm, assign_user_id: v })}>
                  <SelectTrigger><SelectValue placeholder="默认为自己" /></SelectTrigger>
                  <SelectContent>
                    {users.map((u) => (
                      <SelectItem key={u.id} value={String(u.id)}>{u.username}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">留空则令牌归属于当前用户</p>
              </div>
            )}
            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label htmlFor="ct-quota">额度（元）</Label>
                <Input id="ct-quota" type="number" step="0.000001" value={createForm.remain_quota}
                  onChange={(e) => setCreateForm({ ...createForm, remain_quota: e.target.value })}
                  disabled={createForm.unlimited_quota} />
              </div>
              <div className="flex items-end gap-2 pb-2">
                <Switch id="ct-unlimited" checked={createForm.unlimited_quota}
                  onCheckedChange={(v) => setCreateForm({ ...createForm, unlimited_quota: v })} />
                <Label htmlFor="ct-unlimited">无限额度</Label>
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="ct-expired">过期时间</Label>
              <Input id="ct-expired" type="datetime-local" value={createForm.expired_time}
                onChange={(e) => setCreateForm({ ...createForm, expired_time: e.target.value })} />
              <p className="text-xs text-muted-foreground">留空表示永不过期</p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="ct-models">模型限制</Label>
              <Input id="ct-models" value={createForm.model_limits}
                onChange={(e) => setCreateForm({ ...createForm, model_limits: e.target.value })}
                placeholder="gpt-4, claude-3-opus, ..." />
              <div className="flex items-center gap-2">
                <Switch id="ct-models-en" checked={createForm.model_limits_enabled}
                  onCheckedChange={(v) => setCreateForm({ ...createForm, model_limits_enabled: v })} />
                <Label htmlFor="ct-models-en">启用模型限制</Label>
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="ct-ips">IP 白名单</Label>
              <Input id="ct-ips" value={createForm.allow_ips}
                onChange={(e) => setCreateForm({ ...createForm, allow_ips: e.target.value })}
                placeholder="192.168.1.1, 10.0.0.0/24, ..." />
              <p className="text-xs text-muted-foreground">逗号分隔，留空不限制</p>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)} disabled={submitting}>取消</Button>
            <Button onClick={handleCreate} disabled={submitting}>
              {submitting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              创建
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Edit Dialog */}
      <Dialog open={!!editTarget} onOpenChange={(v) => !v && setEditTarget(null)}>
        <DialogContent className="max-h-[90vh] max-w-lg overflow-y-auto">
          <DialogHeader>
            <DialogTitle>编辑令牌</DialogTitle>
            <DialogDescription>修改令牌信息</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="et-name">名称</Label>
              <Input id="et-name" value={editForm.name}
                onChange={(e) => setEditForm({ ...editForm, name: e.target.value })} />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>状态</Label>
                <Select value={editForm.status} onValueChange={(v) => setEditForm({ ...editForm, status: v })}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {STATUS_OPTIONS.map((o) => (
                      <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="flex items-end gap-2 pb-2">
                <Switch id="et-unlimited" checked={editForm.unlimited_quota}
                  onCheckedChange={(v) => setEditForm({ ...editForm, unlimited_quota: v })} />
                <Label htmlFor="et-unlimited">无限额度</Label>
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="et-quota">剩余额度（元）</Label>
              <Input id="et-quota" type="number" step="0.000001" value={editForm.remain_quota}
                onChange={(e) => setEditForm({ ...editForm, remain_quota: e.target.value })}
                disabled={editForm.unlimited_quota} />
            </div>
            <div className="space-y-2">
              <Label htmlFor="et-expired">过期时间</Label>
              <Input id="et-expired" type="datetime-local" value={editForm.expired_time}
                onChange={(e) => setEditForm({ ...editForm, expired_time: e.target.value })} />
              <p className="text-xs text-muted-foreground">留空表示永不过期</p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="et-models">模型限制</Label>
              <Input id="et-models" value={editForm.model_limits}
                onChange={(e) => setEditForm({ ...editForm, model_limits: e.target.value })}
                placeholder="gpt-4, claude-3-opus, ..." />
              <div className="flex items-center gap-2">
                <Switch id="et-models-en" checked={editForm.model_limits_enabled}
                  onCheckedChange={(v) => setEditForm({ ...editForm, model_limits_enabled: v })} />
                <Label htmlFor="et-models-en">启用模型限制</Label>
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="et-ips">IP 白名单</Label>
              <Input id="et-ips" value={editForm.allow_ips}
                onChange={(e) => setEditForm({ ...editForm, allow_ips: e.target.value })}
                placeholder="192.168.1.1, 10.0.0.0/24, ..." />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setEditTarget(null)} disabled={submitting}>取消</Button>
            <Button onClick={handleUpdate} disabled={submitting}>
              {submitting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Confirmation */}
      <Dialog open={!!deleteTarget} onOpenChange={(v) => !v && setDeleteTarget(null)}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>确认删除</DialogTitle>
            <DialogDescription>
              确定要删除令牌 <span className="font-semibold text-foreground">{deleteTarget?.name || deleteTarget?.key.slice(0, 10) + "..."}</span> 吗？此操作不可撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)} disabled={submitting}>取消</Button>
            <Button variant="destructive" onClick={handleDelete} disabled={submitting}>
              {submitting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

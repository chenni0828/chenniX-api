import { useState, useEffect, useCallback } from "react"
import { UserPlus, Pencil, Trash2, Loader2, ShieldAlert, RefreshCw, KeyRound } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
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
import { userApi, type UserConfig, type CreateUserPayload, type UpdateUserPayload } from "@/lib/api/users"
import { formatNumber, formatQuota, getErrorMessage } from "@/lib/format"

const ROLE_OPTIONS = [
  { value: "1", label: "普通用户" },
  { value: "10", label: "管理员" },
  { value: "100", label: "超级管理员" },
]

const STATUS_OPTIONS = [
  { value: "1", label: "启用" },
  { value: "2", label: "禁用" },
]

function roleBadge(role: number) {
  if (role >= 100) return <Badge variant="destructive">超级管理员</Badge>
  if (role >= 10) return <Badge variant="warning">管理员</Badge>
  return <Badge>普通用户</Badge>
}

function statusBadge(status: number) {
  return status === 1
    ? <Badge variant="success">启用</Badge>
    : <Badge variant="destructive">禁用</Badge>
}

export default function Users() {
  const currentUser = useAuthStore((s) => s.user)
  const [users, setUsers] = useState<UserConfig[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")

  const [createOpen, setCreateOpen] = useState(false)
  const [editTarget, setEditTarget] = useState<UserConfig | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<UserConfig | null>(null)
  const [submitting, setSubmitting] = useState(false)

  // Reset password state
  const [passwordTarget, setPasswordTarget] = useState<UserConfig | null>(null)
  const [passwordForm, setPasswordForm] = useState("")
  const [resettingPassword, setResettingPassword] = useState(false)

  const fetchUsers = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const data = await userApi.list()
      setUsers(data)
    } catch (err) {
      setError(getErrorMessage(err, "加载用户列表失败"))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    if (currentUser && currentUser.role >= 10) {
      fetchUsers()
    } else {
      setLoading(false)
    }
  }, [fetchUsers, currentUser])

  // ── Create ──
  const [createForm, setCreateForm] = useState({
    username: "", password: "", role: "1", group: "default", quota: "1000000",
  })

  const handleCreate = async () => {
    if (!createForm.username || !createForm.password) {
      toast({ title: "请填写用户名和密码", variant: "destructive" })
      return
    }
    setSubmitting(true)
    try {
      const payload: CreateUserPayload = {
        username: createForm.username,
        password: createForm.password,
        role: parseInt(createForm.role),
        group: createForm.group || "default",
        quota: parseInt(createForm.quota) || 0,
      }
      await userApi.create(payload)
      toast({ title: "用户创建成功" })
      setCreateOpen(false)
      setCreateForm({ username: "", password: "", role: "1", group: "default", quota: "1000000" })
      fetchUsers()
    } catch (err) {
      toast({ title: "创建失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setSubmitting(false)
    }
  }

  // ── Update ──
  const [editForm, setEditForm] = useState({
    username: "", role: "1", status: "1", group: "default", quota: "0",
  })

  const openEdit = (u: UserConfig) => {
    setEditForm({
      username: u.username,
      role: String(u.role),
      status: String(u.status),
      group: u.group,
      quota: String(u.quota),
    })
    setEditTarget(u)
  }

  const handleUpdate = async () => {
    if (!editTarget) return
    setSubmitting(true)
    try {
      const payload: UpdateUserPayload = {
        username: editForm.username,
        role: parseInt(editForm.role),
        status: parseInt(editForm.status),
        group: editForm.group || "default",
        quota: parseInt(editForm.quota) || 0,
      }
      await userApi.update(editTarget.id, payload)
      toast({ title: "用户更新成功" })
      setEditTarget(null)
      fetchUsers()
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
      await userApi.delete(deleteTarget.id)
      toast({ title: "用户已删除" })
      setDeleteTarget(null)
      fetchUsers()
    } catch (err) {
      toast({ title: "删除失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setSubmitting(false)
    }
  }

  // ── Reset password (admin) ──
  const handleResetPassword = async () => {
    if (!passwordTarget) return
    if (!passwordForm) {
      toast({ title: "请输入新密码", variant: "destructive" })
      return
    }
    setResettingPassword(true)
    try {
      await userApi.updatePassword(passwordTarget.id, passwordForm)
      toast({ title: "密码已重置" })
      setPasswordTarget(null)
      setPasswordForm("")
    } catch (err) {
      toast({ title: "重置失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setResettingPassword(false)
    }
  }

  // ── Permission check ──
  if (!currentUser || currentUser.role < 10) {
    return (
      <div className="space-y-6">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">用户管理</h1>
          <p className="mt-1 text-sm text-muted-foreground">管理系统用户和权限</p>
        </div>
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <ShieldAlert className="mb-4 h-12 w-12 text-muted-foreground" />
            <p className="text-lg font-medium text-muted-foreground">无访问权限</p>
            <p className="mt-1 text-sm text-muted-foreground/70">仅管理员可访问此页面</p>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">用户管理</h1>
          <p className="mt-1 text-sm text-muted-foreground">管理系统用户和权限</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={fetchUsers} disabled={loading}>
            <RefreshCw className={`h-4 w-4 ${loading ? "animate-spin" : ""}`} />
            刷新
          </Button>
          <Button size="sm" onClick={() => setCreateOpen(true)}>
            <UserPlus className="h-4 w-4" />
            新建用户
          </Button>
        </div>
      </div>

      {/* Error */}
      {error && (
        <div className="rounded-md bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {error}
        </div>
      )}

      {/* Table */}
      <Card>
        <CardContent className="p-0">
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : users.length === 0 ? (
            <div className="flex items-center justify-center py-16 text-sm text-muted-foreground">
              暂无用户数据
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="min-w-[100px]">用户名</TableHead>
                  <TableHead>角色</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead>分组</TableHead>
                  <TableHead className="min-w-[160px]">额度使用</TableHead>
                  <TableHead className="text-right">请求数</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {users.map((u) => {
                  const q = formatQuota(u.used_quota, u.quota)
                  return (
                    <TableRow key={u.id}>
                      <TableCell className="font-medium">{u.username}</TableCell>
                      <TableCell>{roleBadge(u.role)}</TableCell>
                      <TableCell>{statusBadge(u.status)}</TableCell>
                      <TableCell>
                        <code className="rounded bg-muted px-1.5 py-0.5 text-xs">{u.group}</code>
                      </TableCell>
                      <TableCell>
                        <div className="space-y-1">
                          <div className="flex items-center gap-2">
                            <div className="h-2 w-24 overflow-hidden rounded-full bg-muted">
                              <div
                                className={`h-full rounded-full ${q.percent >= 90 ? "bg-destructive" : q.percent >= 70 ? "bg-amber-500" : "bg-emerald-500"}`}
                                style={{ width: `${q.percent}%` }}
                              />
                            </div>
                            <span className="text-xs text-muted-foreground">{q.percent}%</span>
                          </div>
                          <span className="text-xs text-muted-foreground">{q.text}</span>
                        </div>
                      </TableCell>
                      <TableCell className="text-right tabular-nums">
                        {formatNumber(u.request_count)}
                      </TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-8 w-8"
                            onClick={() => openEdit(u)}
                            disabled={u.id === currentUser?.id && u.role >= 100}
                            title={u.id === currentUser?.id && u.role >= 100 ? "不能编辑自己的超级管理员账户" : "编辑"}
                          >
                            <Pencil className="h-4 w-4" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-8 w-8"
                            onClick={() => {
                              setPasswordTarget(u)
                              setPasswordForm("")
                            }}
                            title="重置密码"
                          >
                            <KeyRound className="h-4 w-4" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-8 w-8 text-destructive hover:text-destructive"
                            onClick={() => setDeleteTarget(u)}
                            disabled={u.id === currentUser?.id}
                            title={u.id === currentUser?.id ? "不能删除自己" : "删除"}
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
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>新建用户</DialogTitle>
            <DialogDescription>创建一个新的系统用户</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="cu-username">用户名</Label>
              <Input
                id="cu-username"
                value={createForm.username}
                onChange={(e) => setCreateForm({ ...createForm, username: e.target.value })}
                placeholder="输入用户名"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="cu-password">密码</Label>
              <Input
                id="cu-password"
                type="password"
                value={createForm.password}
                onChange={(e) => setCreateForm({ ...createForm, password: e.target.value })}
                placeholder="输入密码"
              />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>角色</Label>
                <Select value={createForm.role} onValueChange={(v) => setCreateForm({ ...createForm, role: v })}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {ROLE_OPTIONS.map((o) => (
                      <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-2">
                <Label htmlFor="cu-group">分组</Label>
                <Input
                  id="cu-group"
                  value={createForm.group}
                  onChange={(e) => setCreateForm({ ...createForm, group: e.target.value })}
                  placeholder="default"
                />
              </div>
            </div>
            <div className="space-y-2">
              <Label htmlFor="cu-quota">额度</Label>
              <Input
                id="cu-quota"
                type="number"
                value={createForm.quota}
                onChange={(e) => setCreateForm({ ...createForm, quota: e.target.value })}
                placeholder="1000000"
              />
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
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>编辑用户</DialogTitle>
            <DialogDescription>修改用户信息</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="eu-username">用户名</Label>
              <Input
                id="eu-username"
                value={editForm.username}
                onChange={(e) => setEditForm({ ...editForm, username: e.target.value })}
              />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>角色</Label>
                <Select value={editForm.role} onValueChange={(v) => setEditForm({ ...editForm, role: v })}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {ROLE_OPTIONS.map((o) => (
                      <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
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
            </div>
            <div className="space-y-2">
              <Label htmlFor="eu-group">分组</Label>
              <Input
                id="eu-group"
                value={editForm.group}
                onChange={(e) => setEditForm({ ...editForm, group: e.target.value })}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="eu-quota">额度</Label>
              <Input
                id="eu-quota"
                type="number"
                value={editForm.quota}
                onChange={(e) => setEditForm({ ...editForm, quota: e.target.value })}
              />
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
              确定要删除用户 <span className="font-semibold text-foreground">{deleteTarget?.username}</span> 吗？此操作不可撤销。
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

      {/* Reset Password Dialog */}
      <Dialog open={!!passwordTarget} onOpenChange={(v) => {
        if (!v) {
          setPasswordTarget(null)
          setPasswordForm("")
        }
      }}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>重置密码</DialogTitle>
            <DialogDescription>
              为用户 <span className="font-semibold text-foreground">{passwordTarget?.username}</span> 设置新密码。
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="reset-password">新密码</Label>
            <Input
              id="reset-password"
              type="password"
              value={passwordForm}
              onChange={(e) => setPasswordForm(e.target.value)}
              placeholder="输入新密码"
              onKeyDown={(e) => { if (e.key === "Enter") handleResetPassword() }}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => { setPasswordTarget(null); setPasswordForm("") }} disabled={resettingPassword}>取消</Button>
            <Button onClick={handleResetPassword} disabled={resettingPassword}>
              {resettingPassword && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              确认重置
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

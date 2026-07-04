import { Fragment, useEffect, useState, useCallback } from "react"
import {
  Plus, RefreshCw, Pencil, Trash2, Key, ArrowLeft, Loader2, Server,
  HelpCircle, ChevronDown, ChevronRight, Zap, Copy, Check,
  Gauge, RotateCcw, Search,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Card, CardContent } from "@/components/ui/card"
import { Badge } from "@/components/ui/badge"
import { Switch } from "@/components/ui/switch"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter,
} from "@/components/ui/dialog"
import {
  Table, TableHeader, TableBody, TableHead, TableRow, TableCell,
} from "@/components/ui/table"
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select"
import {
  channelApi,
  type ChannelConfig, type KeyConfig,
  type CreateChannelData, type CreateKeyData, type UpdateKeyData,
} from "@/lib/api/channels"
import {
  modelApi,
  type SmallModel, type QuotaUnit, type QuotaWindow,
} from "@/lib/api/models"
import { toast } from "@/hooks/use-toast"

// ===== Constants & Helpers =====

const PROVIDERS = [
  { value: "openai-compatible", label: "OpenAI 兼容" },
  { value: "anthropic", label: "Anthropic" },
]

const KEY_STATUS_OPTIONS = [
  { value: "active", label: "活跃" },
  { value: "disabled", label: "禁用" },
  { value: "cooldown", label: "冷却" },
  { value: "quota_exhausted", label: "额度耗尽" },
]

function statusDisplay(status: string): { label: string; variant: "default" | "success" | "warning" | "destructive" | "secondary" } {
  switch (status) {
    case "active": return { label: "活跃", variant: "success" }
    case "cooldown": return { label: "冷却", variant: "warning" }
    case "disabled": return { label: "禁用", variant: "secondary" }
    case "quota_exhausted": return { label: "额度耗尽", variant: "destructive" }
    default: return { label: status, variant: "default" }
  }
}

function statusToValue(status: string): string {
  switch (status) {
    case "active": return "active"
    case "disabled": return "disabled"
    case "cooldown": return "cooldown"
    case "quota_exhausted": return "quota_exhausted"
    default: return "active"
  }
}

function maskKey(_key: string): string {
  if (_key.length <= 12) return _key
  return _key.slice(0, 8) + "..." + _key.slice(-4)
}

// ===== Types =====

interface ChannelFormState {
  name: string
  provider: string
  base_url: string
  api_key: string
  group: string
}

interface KeyFormState {
  api_key: string
  is_free: boolean
  priority: number
  quota_limit: number
  price_per_1k_tokens: number
  status: string
}

const emptyChannelForm: ChannelFormState = {
  name: "", provider: "openai-compatible", base_url: "", api_key: "", group: "default",
}

const emptyKeyForm: KeyFormState = {
  api_key: "", is_free: false, priority: 0, quota_limit: 0, price_per_1k_tokens: 0, status: "active",
}

// ===== Main Component =====

export default function Channels() {
  const [channels, setChannels] = useState<ChannelConfig[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")

  // Channel dialog state
  const [channelDialogOpen, setChannelDialogOpen] = useState(false)
  const [editingChannel, setEditingChannel] = useState<ChannelConfig | null>(null)
  const [channelForm, setChannelForm] = useState<ChannelFormState>(emptyChannelForm)
  const [savingChannel, setSavingChannel] = useState(false)

  // Delete channel state
  const [deleteTarget, setDeleteTarget] = useState<ChannelConfig | null>(null)
  const [deleting, setDeleting] = useState(false)

  // Key management state
  const [keyDialogOpen, setKeyDialogOpen] = useState(false)
  const [keyChannel, setKeyChannel] = useState<ChannelConfig | null>(null)
  const [keys, setKeys] = useState<KeyConfig[]>([])
  const [keysLoading, setKeysLoading] = useState(false)
  const [keyView, setKeyView] = useState<"list" | "form">("list")
  const [editingKey, setEditingKey] = useState<KeyConfig | null>(null)
  const [keyForm, setKeyForm] = useState<KeyFormState>(emptyKeyForm)
  const [savingKey, setSavingKey] = useState(false)
  const [deleteKeyTarget, setDeleteKeyTarget] = useState<KeyConfig | null>(null)
  const [deletingKey, setDeletingKey] = useState(false)
  const [resettingKey, setResettingKey] = useState<Set<number>>(new Set())
  const [copiedKeyId, setCopiedKeyId] = useState<number | null>(null)

  // Reload state
  const [reloading, setReloading] = useState(false)

  // Expand / model list state
  const [expandedRows, setExpandedRows] = useState<Set<number>>(new Set())
  const [discoveringChannels, setDiscoveringChannels] = useState<Set<number>>(new Set())

  // Test channel state
  const [testingChannels, setTestingChannels] = useState<Set<number>>(new Set())
  // 测试结果只通过右下角 toast 提示，不再在表格内 inline 显示。

  // Small model quota state
  const [smallModelsMap, setSmallModelsMap] = useState<Record<string, SmallModel>>({})
  const [smallModelsLoading, setSmallModelsLoading] = useState(false)

  // Quota edit dialog state
  const [quotaOpen, setQuotaOpen] = useState(false)
  const [quotaChannel, setQuotaChannel] = useState<ChannelConfig | null>(null)
  const [quotaModel, setQuotaModel] = useState<SmallModel | null>(null)
  const [quotaForm, setQuotaForm] = useState<{ limit: string; unit: QuotaUnit; window: QuotaWindow; unlimited: boolean }>({
    limit: "", unit: "token", window: "month", unlimited: true,
  })
  const [quotaSaving, setQuotaSaving] = useState(false)
  const [resettingQuota, setResettingQuota] = useState<Set<string>>(new Set())
  // 删除发现模型（小模型池成员）的进行中状态
  const [deletingDiscovered, setDeletingDiscovered] = useState<Set<string>>(new Set())

  // Discover-models selection dialog state
  const [discoverDialogOpen, setDiscoverDialogOpen] = useState(false)
  const [discoverChannel, setDiscoverChannel] = useState<ChannelConfig | null>(null)
  const [discoveredList, setDiscoveredList] = useState<string[]>([])
  const [discoveredSelected, setDiscoveredSelected] = useState<Set<string>>(new Set())
  const [addingDiscovered, setAddingDiscovered] = useState(false)

  const fetchChannels = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const data = await channelApi.list()
      setChannels(data)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "加载渠道列表失败"
      setError(msg)
    } finally {
      setLoading(false)
    }
  }, [])

  const fetchSmallModels = useCallback(async () => {
    setSmallModelsLoading(true)
    try {
      const data = await modelApi.listSmallModels()
      const map: Record<string, SmallModel> = {}
      for (const sm of data) {
        map[`${sm.channel_id}|${sm.raw_model_name}`] = sm
      }
      setSmallModelsMap(map)
    } catch {
      // ignore — quota column will fall back to "无限制"
    } finally {
      setSmallModelsLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchChannels()
    fetchSmallModels()
  }, [fetchChannels, fetchSmallModels])

  // ===== Expand / Model handlers =====

  const toggleRow = async (ch: ChannelConfig) => {
    const newExpanded = new Set(expandedRows)
    if (newExpanded.has(ch.id)) {
      newExpanded.delete(ch.id)
      setExpandedRows(newExpanded)
    } else {
      newExpanded.add(ch.id)
      setExpandedRows(newExpanded)
    }
  }

  const handleDiscoverModels = async (ch: ChannelConfig) => {
    setDiscoveringChannels(prev => new Set(prev).add(ch.id))
    try {
      const result = await channelApi.discoverModelsByChannel(ch.id)
      const all = result.models
      if (all.length === 0) {
        toast({ title: "上游未返回任何模型" })
        return
      }
      // Split into already-in-pool vs new. The pool is keyed by `${channel_id}|${raw_model_name}`.
      const newModels = all.filter(name => !(`${ch.id}|${name}` in smallModelsMap))
      if (newModels.length === 0) {
        toast({ title: "无新增模型", description: `${all.length} 个模型均已在小模型池中` })
        return
      }
      // Open the selection dialog with all new models pre-checked.
      setDiscoverChannel(ch)
      setDiscoveredList(all)
      setDiscoveredSelected(new Set(newModels))
      setDiscoverDialogOpen(true)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "获取模型失败"
      toast({ title: `${ch.name} 获取失败`, description: msg, variant: "destructive" })
    } finally {
      setDiscoveringChannels(prev => {
        const next = new Set(prev)
        next.delete(ch.id)
        return next
      })
    }
  }

  const handleConfirmAddDiscovered = async () => {
    if (!discoverChannel || discoveredSelected.size === 0) return
    setAddingDiscovered(true)
    try {
      const result = await channelApi.addDiscoveredModels(
        discoverChannel.id,
        [...discoveredSelected],
      )
      toast({ title: `已加入 ${result.added} 个模型` })
      setDiscoverDialogOpen(false)
      setDiscoveredList([])
      setDiscoveredSelected(new Set())
      setDiscoverChannel(null)
      await fetchSmallModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "加入失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setAddingDiscovered(false)
    }
  }

  // ===== Test channel handler =====

  const handleTestChannel = async (ch: ChannelConfig) => {
    setTestingChannels(prev => new Set(prev).add(ch.id))
    try {
      const result = await channelApi.testChannel(ch.id)
      if (result.success) {
        toast({ title: `${ch.name} 连接成功`, description: `延迟 ${result.latency_ms}ms` })
      } else {
        toast({ title: `${ch.name} 连接失败`, description: result.error || "未知错误", variant: "destructive" })
      }
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "测试请求失败"
      toast({ title: `${ch.name} 测试失败`, description: msg, variant: "destructive" })
    } finally {
      setTestingChannels(prev => {
        const next = new Set(prev)
        next.delete(ch.id)
        return next
      })
    }
  }

  // ===== Small model quota helpers =====

  const formatQuotaNumber = (n: number): string => {
    if (n >= 10000) {
      const wan = n / 10000
      return (wan % 1 === 0 ? wan.toFixed(0) : wan.toFixed(1)) + "万"
    }
    return n.toLocaleString("en-US")
  }

  const unitLabel = (u: QuotaUnit): string => (u === "token" ? "token" : "次")

  const windowLabel = (w: QuotaWindow): string => {
    switch (w) {
      case "day": return "日"
      case "month": return "月"
      case "total": return "total"
      default: return w
    }
  }

  const openQuota = (ch: ChannelConfig, sm: SmallModel) => {
    setQuotaChannel(ch)
    setQuotaModel(sm)
    setQuotaForm({
      limit: sm.quota_limit != null ? String(sm.quota_limit) : "",
      unit: sm.quota_unit ?? "token",
      window: sm.quota_window ?? "month",
      unlimited: sm.quota_limit == null,
    })
    setQuotaOpen(true)
  }

  const handleSaveQuota = async () => {
    if (!quotaChannel || !quotaModel) return
    const upstream = quotaModel.raw_model_name
    // 无限制模式：直接发 limit: null
    if (quotaForm.unlimited) {
      setQuotaSaving(true)
      try {
        await modelApi.updateSmallModelQuota(quotaChannel.id, upstream, {
          limit: null,
          unit: quotaForm.unit,
          window: quotaForm.window,
        })
        toast({ title: "额度已保存" })
        setQuotaOpen(false)
        fetchSmallModels()
      } catch (err: unknown) {
        const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存失败"
        toast({ title: msg, variant: "destructive" })
      } finally {
        setQuotaSaving(false)
      }
      return
    }
    // 有限额度模式：校验数字
    const limitNum = parseInt(quotaForm.limit, 10)
    if (!quotaForm.limit.trim() || isNaN(limitNum) || limitNum <= 0) {
      toast({ title: "请输入有效的额度上限", variant: "destructive" })
      return
    }
    setQuotaSaving(true)
    try {
      await modelApi.updateSmallModelQuota(quotaChannel.id, upstream, {
        limit: limitNum,
        unit: quotaForm.unit,
        window: quotaForm.window,
      })
      toast({ title: "额度已保存" })
      setQuotaOpen(false)
      fetchSmallModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setQuotaSaving(false)
    }
  }

  const handleResetQuota = async (ch: ChannelConfig, sm: SmallModel) => {
    const upstream = sm.raw_model_name
    const key = `${ch.id}|${upstream}`
    setResettingQuota(prev => new Set(prev).add(key))
    try {
      await modelApi.resetSmallModelQuota(ch.id, upstream)
      toast({ title: "额度已重置" })
      fetchSmallModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "重置失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setResettingQuota(prev => {
        const next = new Set(prev)
        next.delete(key)
        return next
      })
    }
  }

  // 从小模型池移除指定发现模型。binding_count>0 时后端会拒绝，
  // 但前端也同步灰掉按钮并给出 tooltip 提示。
  const handleDeleteDiscovered = async (ch: ChannelConfig, sm: SmallModel) => {
    if (sm.binding_count > 0) {
      toast({
        title: `该模型已被 ${sm.binding_count} 个大模型绑定，请先在 Models 页面解除绑定`,
        variant: "destructive",
      })
      return
    }
    if (!window.confirm(`确定要从小模型池移除「${sm.raw_model_name}」吗？`)) return
    const key = `${ch.id}|${sm.raw_model_name}`
    setDeletingDiscovered(prev => new Set(prev).add(key))
    try {
      await channelApi.deleteDiscoveredModel(ch.id, sm.raw_model_name)
      toast({ title: "已移除" })
      fetchSmallModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "移除失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setDeletingDiscovered(prev => {
        const next = new Set(prev)
        next.delete(key)
        return next
      })
    }
  }

  const renderQuotaCell = (ch: ChannelConfig, sm: SmallModel) => {
    const resetKey = `${ch.id}|${sm.raw_model_name}`
    const isResetting = resettingQuota.has(resetKey)
    if (sm.quota_limit == null) {
      return (
        <div className="flex items-center gap-1.5">
          <Badge variant="secondary">无限制</Badge>
          <Button
            size="sm"
            variant="outline"
            className="h-6 border-dashed px-2 text-xs"
            onClick={() => openQuota(ch, sm)}
          >
            <Gauge className="mr-1 h-3 w-3" /> 设置配额
          </Button>
        </div>
      )
    }
    const unit = sm.quota_unit ?? "token"
    const window = sm.quota_window ?? "total"
    const text = `${formatQuotaNumber(sm.used_quota)} / ${formatQuotaNumber(sm.quota_limit)} ${unitLabel(unit)} /${windowLabel(window)}`
    if (sm.quota_status === "exhausted") {
      return (
        <div className="flex items-center gap-1.5">
          <Badge variant="destructive">已耗尽</Badge>
          <span className="text-xs text-muted-foreground">{text}</span>
          {window === "total" && (
            <Button
              size="sm"
              variant="outline"
              className="h-6 px-2 text-xs"
              onClick={() => handleResetQuota(ch, sm)}
              disabled={isResetting}
            >
              {isResetting
                ? <Loader2 className="h-3 w-3 animate-spin" />
                : <RotateCcw className="mr-1 h-3 w-3" />}
              重置
            </Button>
          )}
          <Button
            size="sm"
            variant="ghost"
            className="h-6 w-6 p-0"
            onClick={() => openQuota(ch, sm)}
            title="编辑额度"
          >
            <Gauge className="h-3 w-3" />
          </Button>
        </div>
      )
    }
    return (
      <div className="flex items-center gap-1.5">
        <Badge variant="success">可用</Badge>
        <span className="text-xs text-muted-foreground">{text}</span>
        <Button
          size="sm"
          variant="ghost"
          className="h-6 w-6 p-0"
          onClick={() => openQuota(ch, sm)}
          title="编辑额度"
        >
          <Gauge className="h-3 w-3" />
        </Button>
      </div>
    )
  }

  // ===== Channel handlers =====

  const openCreateChannel = () => {
    setEditingChannel(null)
    setChannelForm(emptyChannelForm)
    setChannelDialogOpen(true)
  }

  const openEditChannel = async (ch: ChannelConfig) => {
    setEditingChannel(ch)
    setChannelForm({
      name: ch.name,
      provider: ch.provider,
      base_url: ch.base_url,
      api_key: "",
      group: ch.group,
    })
    setChannelDialogOpen(true)
    // Pre-fill the first available API key for convenience
    try {
      const keys = await channelApi.listKeys(ch.id)
      const firstActive = keys.find(k => k.status === "active")
      const key = (firstActive || keys[0])?.api_key || ""
      setChannelForm(f => ({ ...f, api_key: key }))
    } catch {
      // ignore
    }
  }

  const handleSaveChannel = async () => {
    if (!channelForm.name.trim()) {
      toast({ title: "请输入渠道名称", variant: "destructive" })
      return
    }
    if (!channelForm.base_url.trim()) {
      toast({ title: "请输入 Base URL", variant: "destructive" })
      return
    }
    setSavingChannel(true)
    try {
      const data: CreateChannelData = {
        name: channelForm.name.trim(),
        provider: channelForm.provider,
        base_url: channelForm.base_url.trim(),
        group: channelForm.group.trim() || "default",
      }
      let channelId: number
      if (editingChannel) {
        await channelApi.update(editingChannel.id, data)
        channelId = editingChannel.id
      } else {
        channelId = await channelApi.create(data)
      }

      // Create a key if api_key is provided and doesn't already exist
      if (channelForm.api_key.trim()) {
        try {
          const existingKeys = await channelApi.listKeys(channelId)
          const keyExists = existingKeys.some(k => k.api_key === channelForm.api_key.trim())
          if (!keyExists) {
            await channelApi.createKey(channelId, {
              api_key: channelForm.api_key.trim(),
              is_free: false,
              priority: 0,
              quota_limit: 0,
              price_per_1k_tokens: 0,
            })
          }
        } catch {
          // Key creation failed — channel was still saved successfully
        }
      }

      toast({ title: editingChannel ? "渠道更新成功" : "渠道创建成功" })
      setChannelDialogOpen(false)
      fetchChannels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setSavingChannel(false)
    }
  }

  const handleDeleteChannel = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await channelApi.delete(deleteTarget.id)
      toast({ title: "渠道已删除" })
      setDeleteTarget(null)
      fetchChannels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "删除失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setDeleting(false)
    }
  }

  const handleReload = async () => {
    setReloading(true)
    try {
      await channelApi.reload()
      // 同步刷新前端 state，避免跨页操作后本地数据陈旧
      // （例如 Models 页解绑后 smallModelsMap.binding_count 不刷新会导致删除按钮死锁）
      await Promise.all([fetchChannels(), fetchSmallModels()])
      toast({ title: "缓存已刷新" })
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "刷新失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setReloading(false)
    }
  }

  // ===== Key handlers =====

  const openKeyManager = async (ch: ChannelConfig) => {
    setKeyChannel(ch)
    setKeyView("list")
    setKeyDialogOpen(true)
    setKeysLoading(true)
    try {
      const data = await channelApi.listKeys(ch.id)
      setKeys(data)
    } catch {
      setKeys([])
    } finally {
      setKeysLoading(false)
    }
  }

  const refreshKeys = async (channelId: number) => {
    try {
      const data = await channelApi.listKeys(channelId)
      setKeys(data)
    } catch {
      // ignore
    }
  }

  const openCreateKey = () => {
    setEditingKey(null)
    setKeyForm(emptyKeyForm)
    setKeyView("form")
  }

  const openEditKey = (key: KeyConfig) => {
    setEditingKey(key)
    setKeyForm({
      api_key: key.api_key,
      is_free: key.cost_tier === "free",
      priority: key.key_priority,
      quota_limit: key.free_quota ?? 0,
      price_per_1k_tokens: key.price_per_1k_tokens ?? 0,
      status: statusToValue(key.status),
    })
    setKeyView("form")
  }

  const handleSaveKey = async () => {
    if (!keyChannel) return
    if (!keyForm.api_key.trim()) {
      toast({ title: "请输入 API Key", variant: "destructive" })
      return
    }
    setSavingKey(true)
    try {
      if (editingKey) {
        const data: UpdateKeyData = {
          api_key: keyForm.api_key.trim(),
          is_free: keyForm.is_free,
          priority: keyForm.priority,
          quota_limit: keyForm.quota_limit,
          price_per_1k_tokens: keyForm.price_per_1k_tokens,
          status: keyForm.status,
        }
        await channelApi.updateKey(keyChannel.id, editingKey.id, data)
        toast({ title: "Key 更新成功" })
      } else {
        const data: CreateKeyData = {
          api_key: keyForm.api_key.trim(),
          is_free: keyForm.is_free,
          priority: keyForm.priority,
          quota_limit: keyForm.quota_limit,
          price_per_1k_tokens: keyForm.price_per_1k_tokens,
        }
        await channelApi.createKey(keyChannel.id, data)
        toast({ title: "Key 创建成功" })
      }
      setKeyView("list")
      refreshKeys(keyChannel.id)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setSavingKey(false)
    }
  }

  const handleDeleteKey = async () => {
    if (!keyChannel || !deleteKeyTarget) return
    setDeletingKey(true)
    try {
      await channelApi.deleteKey(keyChannel.id, deleteKeyTarget.id)
      toast({ title: "Key 已删除" })
      setDeleteKeyTarget(null)
      refreshKeys(keyChannel.id)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "删除失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setDeletingKey(false)
    }
  }

  const handleResetKeyQuota = async (channelId: number, key: KeyConfig) => {
    setResettingKey(prev => new Set(prev).add(key.id))
    try {
      await channelApi.resetKeyQuota(channelId, key.id)
      toast({ title: "Key 用量已重置" })
      await refreshKeys(channelId)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "重置失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setResettingKey(prev => {
        const next = new Set(prev)
        next.delete(key.id)
        return next
      })
    }
  }

  // ===== Render =====

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center py-20">
        <div className="flex flex-col items-center gap-3">
          <div className="h-8 w-8 animate-spin rounded-full border-2 border-primary border-t-transparent" />
          <p className="text-sm text-muted-foreground">加载中...</p>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="space-y-6">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">渠道管理</h1>
          <p className="text-sm text-muted-foreground mt-1">管理上游 AI API 渠道</p>
        </div>
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <p className="text-lg font-medium text-destructive">{error}</p>
            <Button variant="outline" className="mt-4" onClick={fetchChannels}>重试</Button>
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
          <h1 className="text-2xl font-bold tracking-tight">渠道管理</h1>
          <p className="text-sm text-muted-foreground mt-1">管理上游 AI API 渠道</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={handleReload} disabled={reloading}>
            <RefreshCw className={`mr-2 h-4 w-4 ${reloading ? "animate-spin" : ""}`} />
            刷新缓存
          </Button>
          <Button size="sm" onClick={openCreateChannel}>
            <Plus className="mr-2 h-4 w-4" />
            新建渠道
          </Button>
        </div>
      </div>

      {/* Channel Table */}
      <Card>
        <CardContent className="p-0">
          {channels.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-16">
              <Server className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-sm text-muted-foreground">暂无渠道，点击「新建渠道」开始</p>
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-8"></TableHead>
                  <TableHead>名称</TableHead>
                  <TableHead>Provider</TableHead>
                  <TableHead>Base URL</TableHead>
                  <TableHead>
                    <span className="inline-flex items-center gap-1">
                      分组
                      <span className="group relative inline-flex cursor-help">
                        <HelpCircle className="h-3.5 w-3.5 text-muted-foreground" />
                        <span className="pointer-events-none absolute bottom-full left-1/2 z-50 mb-2 -translate-x-1/2 whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground opacity-0 shadow-md transition-opacity group-hover:opacity-100">
                          分组用于路由匹配：只有与用户分组匹配的渠道才会被选中处理该用户的请求
                        </span>
                      </span>
                    </span>
                  </TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {channels.map((ch) => {
                  const providerLabel = PROVIDERS.find(p => p.value === ch.provider)?.label ?? ch.provider
                  const isExpanded = expandedRows.has(ch.id)
                  const isTesting = testingChannels.has(ch.id)
                  const isDiscovering = discoveringChannels.has(ch.id)
                  const channelSmallModels = Object.values(smallModelsMap).filter(sm => sm.channel_id === ch.id)
                  return (
                    <Fragment key={ch.id}>
                      <TableRow>
                        <TableCell>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-6 w-6"
                            onClick={() => toggleRow(ch)}
                          >
                            {isExpanded
                              ? <ChevronDown className="h-4 w-4 text-muted-foreground" />
                              : <ChevronRight className="h-4 w-4 text-muted-foreground" />
                            }
                          </Button>
                        </TableCell>
                        <TableCell className="font-medium">{ch.name}</TableCell>
                        <TableCell>
                          <Badge variant="outline">{providerLabel}</Badge>
                        </TableCell>
                        <TableCell className="max-w-[200px] truncate text-muted-foreground" title={ch.base_url}>
                          {ch.base_url}
                        </TableCell>
                        <TableCell>
                          <Badge variant="secondary">{ch.group}</Badge>
                        </TableCell>
                        <TableCell className="text-right">
                          <div className="flex justify-end gap-1">
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => handleTestChannel(ch)}
                              disabled={isTesting}
                              title="测试连接"
                            >
                              {isTesting
                                ? <Loader2 className="h-4 w-4 animate-spin" />
                                : <Zap className="h-4 w-4" />
                              }
                            </Button>
                            <Button variant="ghost" size="icon" onClick={() => openKeyManager(ch)} title="管理 Key">
                              <Key className="h-4 w-4" />
                            </Button>
                            <Button variant="ghost" size="icon" onClick={() => openEditChannel(ch)} title="编辑">
                              <Pencil className="h-4 w-4" />
                            </Button>
                            <Button variant="ghost" size="icon" onClick={() => setDeleteTarget(ch)} title="删除">
                              <Trash2 className="h-4 w-4 text-destructive" />
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                      {/* Expanded row: discovered models with quota column */}
                      {isExpanded && (
                        <TableRow>
                          <TableCell colSpan={6} className="bg-muted/30 p-4">
                            <div className="space-y-3">
                              <div className="flex items-center justify-between">
                                <p className="text-sm font-medium">已发现模型</p>
                                <Button
                                  size="sm"
                                  variant="outline"
                                  onClick={() => handleDiscoverModels(ch)}
                                  disabled={isDiscovering}
                                >
                                  {isDiscovering
                                    ? <Loader2 className="mr-1 h-3 w-3 animate-spin" />
                                    : <Search className="mr-1 h-3 w-3" />}
                                  获取模型
                                </Button>
                              </div>
                              {smallModelsLoading ? (
                                <div className="flex items-center gap-2 py-2">
                                  <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
                                  <span className="text-sm text-muted-foreground">加载模型列表...</span>
                                </div>
                              ) : channelSmallModels.length === 0 ? (
                                <p className="text-sm text-muted-foreground">暂无模型，点击「获取模型」从上游拉取</p>
                              ) : (
                                <div className="flex flex-col gap-1.5">
                                  {channelSmallModels.map((sm) => {
                                    const delKey = `${ch.id}|${sm.raw_model_name}`
                                    const isDeletingDiscovered = deletingDiscovered.has(delKey)
                                    const boundTip = sm.binding_count > 0
                                      ? `已被 ${sm.binding_count} 个大模型绑定，请先在 Models 页面解除绑定`
                                      : "从小模型池移除"
                                    return (
                                    <div
                                      key={sm.raw_model_name}
                                      className="flex items-center justify-between gap-2 rounded-md border px-2.5 py-1.5"
                                    >
                                      <div className="flex items-center gap-2 min-w-0 flex-1">
                                        <span className="font-mono text-xs font-medium truncate">{sm.raw_model_name}</span>
                                        {sm.binding_count > 0 && (
                                          <Badge variant="secondary" className="text-[10px] h-4 px-1">
                                            {sm.binding_count} 绑定
                                          </Badge>
                                        )}
                                      </div>
                                      <div className="shrink-0 flex items-center gap-1">
                                        {renderQuotaCell(ch, sm)}
                                        <Button
                                          size="sm"
                                          variant="ghost"
                                          className="h-6 w-6 p-0 text-destructive hover:text-destructive"
                                          onClick={() => handleDeleteDiscovered(ch, sm)}
                                          disabled={isDeletingDiscovered || sm.binding_count > 0}
                                          title={boundTip}
                                        >
                                          {isDeletingDiscovered
                                            ? <Loader2 className="h-3 w-3 animate-spin" />
                                            : <Trash2 className="h-3 w-3" />}
                                        </Button>
                                      </div>
                                    </div>
                                    )
                                  })}
                                </div>
                              )}
                            </div>
                          </TableCell>
                        </TableRow>
                      )}
                    </Fragment>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      {/* Channel Create/Edit Dialog */}
      <Dialog open={channelDialogOpen} onOpenChange={setChannelDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editingChannel ? "编辑渠道" : "新建渠道"}</DialogTitle>
            <DialogDescription>
              {editingChannel ? "修改渠道配置信息" : "添加一个新的上游 API 渠道"}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="ch-name">名称</Label>
              <Input
                id="ch-name"
                value={channelForm.name}
                onChange={(e) => setChannelForm(f => ({ ...f, name: e.target.value }))}
                placeholder="如：OpenAI 主渠道"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="ch-provider">Provider</Label>
              <Select
                value={channelForm.provider}
                onValueChange={(v) => setChannelForm(f => ({ ...f, provider: v }))}
              >
                <SelectTrigger id="ch-provider">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {PROVIDERS.map(p => (
                    <SelectItem key={p.value} value={p.value}>{p.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-2">
              <Label htmlFor="ch-url">Base URL</Label>
              <Input
                id="ch-url"
                value={channelForm.base_url}
                onChange={(e) => setChannelForm(f => ({ ...f, base_url: e.target.value }))}
                placeholder="https://api.openai.com/v1"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="ch-apikey">API Key</Label>
              <Input
                id="ch-apikey"
                type="password"
                value={channelForm.api_key}
                onChange={(e) => setChannelForm(f => ({ ...f, api_key: e.target.value }))}
                placeholder="sk-..."
              />
              <p className="text-xs text-muted-foreground">保存渠道时将自动创建 Key</p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="ch-group">分组</Label>
              <Input
                id="ch-group"
                value={channelForm.group}
                onChange={(e) => setChannelForm(f => ({ ...f, group: e.target.value }))}
                placeholder="default"
              />
            </div>

          </div>
          <DialogFooter>
            {editingChannel && (
              <Button
                variant="outline"
                className="mr-auto"
                onClick={() => {
                  setChannelDialogOpen(false)
                  openKeyManager(editingChannel)
                }}
              >
                <Key className="mr-2 h-4 w-4" />
                管理 API Key
              </Button>
            )}
            <Button variant="outline" onClick={() => setChannelDialogOpen(false)}>取消</Button>
            <Button onClick={handleSaveChannel} disabled={savingChannel}>
              {savingChannel && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {savingChannel ? "保存中..." : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Channel Confirmation */}
      <Dialog open={!!deleteTarget} onOpenChange={(open) => !open && setDeleteTarget(null)}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>确认删除</DialogTitle>
            <DialogDescription>
              确定要删除渠道「{deleteTarget?.name}」吗？此操作不可撤销，该渠道下的所有 Key 也将被删除。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>取消</Button>
            <Button variant="destructive" onClick={handleDeleteChannel} disabled={deleting}>
              {deleting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {deleting ? "删除中..." : "确认删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Key Management Dialog */}
      <Dialog open={keyDialogOpen} onOpenChange={(open) => {
        setKeyDialogOpen(open)
        if (!open) {
          setKeyChannel(null)
          setKeys([])
          setKeyView("list")
        }
      }}>
        <DialogContent className="max-w-3xl">
          {keyView === "list" ? (
            <>
              <DialogHeader>
                <div className="flex items-center justify-between pr-8">
                  <div className="flex items-center gap-2">
                    <Key className="h-5 w-5" />
                    <DialogTitle>Key 管理 — {keyChannel?.name}</DialogTitle>
                  </div>
                  <Button size="sm" onClick={openCreateKey}>
                    <Plus className="mr-1.5 h-4 w-4" /> 添加 Key
                  </Button>
                </div>
                <DialogDescription>管理该渠道下的 API Key</DialogDescription>
              </DialogHeader>
              {keysLoading ? (
                <div className="flex h-32 items-center justify-center">
                  <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
                </div>
              ) : keys.length === 0 ? (
                <div className="flex h-32 flex-col items-center justify-center gap-3 text-sm text-muted-foreground">
                  <span>暂无 Key</span>
                  <Button size="sm" variant="outline" onClick={openCreateKey}>
                    <Plus className="mr-1.5 h-4 w-4" /> 添加 Key
                  </Button>
                </div>
              ) : (
                <div className="max-h-[400px] overflow-auto">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>API Key</TableHead>
                        <TableHead>类型</TableHead>
                        <TableHead className="text-right">优先级</TableHead>
                        <TableHead className="text-right">额度</TableHead>
                        <TableHead>状态</TableHead>
                        <TableHead className="text-right">操作</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {keys.map((k) => (
                        <TableRow key={k.id}>
                          <TableCell className="font-mono text-xs">
                            <div className="flex items-center gap-1">
                              <span className="break-all">{maskKey(k.api_key)}</span>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-5 w-5 shrink-0"
                                onClick={async () => {
                                  await navigator.clipboard.writeText(k.api_key)
                                  setCopiedKeyId(k.id)
                                  setTimeout(() => setCopiedKeyId(null), 2000)
                                }}
                                title="复制"
                              >
                                {copiedKeyId === k.id
                                  ? <Check className="h-3 w-3 text-green-500" />
                                  : <Copy className="h-3 w-3" />
                                }
                              </Button>
                            </div>
                          </TableCell>
                          <TableCell>
                            <Badge variant={k.cost_tier === "free" ? "success" : "secondary"}>
                              {k.cost_tier === "free" ? "免费" : "付费"}
                            </Badge>
                          </TableCell>
                          <TableCell className="text-right tabular-nums">{k.key_priority}</TableCell>
                          <TableCell className="text-right tabular-nums text-muted-foreground">
                            {k.free_quota ? `${k.used_quota}/${k.free_quota}` : k.used_quota}
                          </TableCell>
                          <TableCell>
                            {(() => {
                              const s = statusDisplay(k.status)
                              return <Badge variant={s.variant}>{s.label}</Badge>
                            })()}
                          </TableCell>
                          <TableCell className="text-right">
                            <div className="flex justify-end gap-1">
                              <Button variant="ghost" size="icon" onClick={() => openEditKey(k)} title="编辑">
                                <Pencil className="h-4 w-4" />
                              </Button>
                              <Button
                                variant="ghost"
                                size="icon"
                                onClick={() => handleResetKeyQuota(keyChannel!.id, k)}
                                disabled={resettingKey.has(k.id)}
                                title="重置用量"
                              >
                                {resettingKey.has(k.id)
                                  ? <Loader2 className="h-4 w-4 animate-spin" />
                                  : <RefreshCw className="h-4 w-4" />}
                              </Button>
                              <Button variant="ghost" size="icon" onClick={() => setDeleteKeyTarget(k)} title="删除">
                                <Trash2 className="h-4 w-4 text-destructive" />
                              </Button>
                            </div>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              )}
            </>
          ) : (
            <>
              <DialogHeader>
                <DialogTitle className="flex items-center gap-2">
                  <Button variant="ghost" size="icon" onClick={() => setKeyView("list")}>
                    <ArrowLeft className="h-4 w-4" />
                  </Button>
                  {editingKey ? "编辑 Key" : "添加 Key"}
                </DialogTitle>
                <DialogDescription>
                  {keyChannel?.name} — {editingKey ? maskKey(editingKey.api_key) : "新建 API Key"}
                </DialogDescription>
              </DialogHeader>
              <div className="space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="key-value">API Key</Label>
                  <Input
                    id="key-value"
                    value={keyForm.api_key}
                    onChange={(e) => setKeyForm(f => ({ ...f, api_key: e.target.value }))}
                    placeholder="sk-..."
                  />
                </div>
                <div className="flex items-center justify-between rounded-lg border p-3">
                  <div>
                    <Label htmlFor="key-free">免费 Key</Label>
                    <p className="text-xs text-muted-foreground mt-1">免费 Key 不计入费用统计</p>
                  </div>
                  <Switch
                    id="key-free"
                    checked={keyForm.is_free}
                    onCheckedChange={(v) => setKeyForm(f => ({ ...f, is_free: v }))}
                  />
                </div>
                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label htmlFor="key-priority">优先级</Label>
                    <Input
                      id="key-priority"
                      type="number"
                      value={keyForm.priority}
                      onChange={(e) => setKeyForm(f => ({ ...f, priority: parseInt(e.target.value) || 0 }))}
                    />
                  </div>
                  {editingKey && (
                    <div className="space-y-2">
                      <Label htmlFor="key-status">状态</Label>
                      <Select
                        value={keyForm.status}
                        onValueChange={(v) => setKeyForm(f => ({ ...f, status: v }))}
                      >
                        <SelectTrigger id="key-status">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {KEY_STATUS_OPTIONS.map(s => (
                            <SelectItem key={s.value} value={String(s.value)}>{s.label}</SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  )}
                </div>
                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label htmlFor="key-quota">额度限制</Label>
                    <Input
                      id="key-quota"
                      type="number"
                      value={keyForm.quota_limit}
                      onChange={(e) => setKeyForm(f => ({ ...f, quota_limit: parseInt(e.target.value) || 0 }))}
                      placeholder="0 = 无限"
                    />
                  </div>
                  <div className="space-y-2">
                    <Label htmlFor="key-price">单价 / 1K Token</Label>
                    <Input
                      id="key-price"
                      type="number"
                      step="0.001"
                      value={keyForm.price_per_1k_tokens}
                      onChange={(e) => setKeyForm(f => ({ ...f, price_per_1k_tokens: parseFloat(e.target.value) || 0 }))}
                      placeholder="0.000"
                    />
                  </div>
                </div>
              </div>
              <DialogFooter>
                <Button variant="outline" onClick={() => setKeyView("list")}>取消</Button>
                <Button onClick={handleSaveKey} disabled={savingKey}>
                  {savingKey && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                  {savingKey ? "保存中..." : "保存"}
                </Button>
              </DialogFooter>
            </>
          )}
        </DialogContent>
      </Dialog>

      {/* Delete Key Confirmation */}
      <Dialog open={!!deleteKeyTarget} onOpenChange={(open) => !open && setDeleteKeyTarget(null)}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>确认删除</DialogTitle>
            <DialogDescription>
              确定要删除 Key「{deleteKeyTarget ? maskKey(deleteKeyTarget.api_key) : ""}」吗？
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteKeyTarget(null)}>取消</Button>
            <Button variant="destructive" onClick={handleDeleteKey} disabled={deletingKey}>
              {deletingKey && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {deletingKey ? "删除中..." : "确认删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Quota Edit Dialog */}
      <Dialog open={quotaOpen} onOpenChange={setQuotaOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>额度配置</DialogTitle>
            <DialogDescription>
              {quotaChannel?.name} · {quotaModel?.raw_model_name}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            {(() => {
              const sm = quotaModel
              if (!sm) return null
              const unit = sm.quota_unit ?? "token"
              return (
                <div className="rounded-md bg-muted/50 p-2.5 text-xs text-muted-foreground space-y-0.5">
                  <p>当前已用：{formatQuotaNumber(sm.used_quota)} {unitLabel(unit)}
                    {sm.quota_limit != null && ` / ${formatQuotaNumber(sm.quota_limit)}`}</p>
                  {sm.last_reset_at && (
                    <p>上次重置：{new Date(sm.last_reset_at).toLocaleString()}</p>
                  )}
                </div>
              )
            })()}
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <Label htmlFor="quota-limit">额度上限</Label>
                <div className="flex items-center gap-2">
                  <Switch
                    checked={quotaForm.unlimited}
                    onCheckedChange={(v) => setQuotaForm(f => ({ ...f, unlimited: v }))}
                  />
                  <span className="text-xs text-muted-foreground">无限制</span>
                </div>
              </div>
              <Input
                id="quota-limit"
                type="number"
                min="1"
                value={quotaForm.limit}
                onChange={(e) => setQuotaForm(f => ({ ...f, limit: e.target.value }))}
                placeholder="如 1000000"
                disabled={quotaForm.unlimited}
              />
            </div>
            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>单位</Label>
                <Select
                  value={quotaForm.unit}
                  onValueChange={(v) => setQuotaForm(f => ({ ...f, unit: v as QuotaUnit }))}
                >
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="token">Token</SelectItem>
                    <SelectItem value="call">调用次数</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-2">
                <Label>重置周期</Label>
                <Select
                  value={quotaForm.window}
                  onValueChange={(v) => setQuotaForm(f => ({ ...f, window: v as QuotaWindow }))}
                >
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="day">每日</SelectItem>
                    <SelectItem value="month">每月</SelectItem>
                    <SelectItem value="total">永不</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setQuotaOpen(false)}>取消</Button>
            <Button onClick={handleSaveQuota} disabled={quotaSaving}>
              {quotaSaving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {quotaSaving ? "保存中..." : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Discover Models Selection Dialog */}
      <Dialog
        open={discoverDialogOpen}
        onOpenChange={(open) => {
          setDiscoverDialogOpen(open)
          if (!open) {
            setDiscoveredList([])
            setDiscoveredSelected(new Set())
            setDiscoverChannel(null)
          }
        }}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>选择要加入的模型 — {discoverChannel?.name}</DialogTitle>
            <DialogDescription>
              共 {discoveredList.length} 个，{discoveredSelected.size} 个新增待加入
            </DialogDescription>
          </DialogHeader>
          <div className="flex items-center justify-between">
            <span className="text-xs text-muted-foreground">已添加的模型将标灰显示，不可重复加入</span>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={() => {
                if (!discoverChannel) return
                const newModels = discoveredList.filter(
                  name => !(`${discoverChannel.id}|${name}` in smallModelsMap),
                )
                const allSelected = newModels.every(n => discoveredSelected.has(n))
                setDiscoveredSelected(allSelected ? new Set() : new Set(newModels))
              }}
            >
              {(() => {
                if (!discoverChannel) return "全选"
                const newModels = discoveredList.filter(
                  name => !(`${discoverChannel.id}|${name}` in smallModelsMap),
                )
                return newModels.length > 0 && newModels.every(n => discoveredSelected.has(n))
                  ? "取消全选"
                  : "全选新增"
              })()}
            </Button>
          </div>
          <div className="max-h-[60vh] overflow-y-auto space-y-1 rounded-md border p-1">
            {discoveredList.map((name) => {
              const exists = discoverChannel
                ? (`${discoverChannel.id}|${name}` in smallModelsMap)
                : false
              const checked = discoveredSelected.has(name)
              return (
                <label
                  key={name}
                  className={`flex items-center gap-2.5 rounded px-2 py-1.5 text-sm transition-colors ${
                    exists ? "opacity-50" : "hover:bg-muted/60 cursor-pointer"
                  }`}
                >
                  <Checkbox
                    checked={exists ? true : checked}
                    disabled={exists}
                    onClick={() => {
                      if (exists) return
                      setDiscoveredSelected(prev => {
                        const next = new Set(prev)
                        if (checked) next.delete(name)
                        else next.add(name)
                        return next
                      })
                    }}
                  />
                  <span className="font-mono text-xs">{name}</span>
                  {exists && (
                    <Badge variant="secondary" className="ml-auto text-[10px]">已添加</Badge>
                  )}
                </label>
              )
            })}
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setDiscoverDialogOpen(false)}
              disabled={addingDiscovered}
            >
              取消
            </Button>
            <Button
              onClick={handleConfirmAddDiscovered}
              disabled={addingDiscovered || discoveredSelected.size === 0}
            >
              {addingDiscovered && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {addingDiscovered ? "加入中..." : `加入所选 (${discoveredSelected.size})`}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

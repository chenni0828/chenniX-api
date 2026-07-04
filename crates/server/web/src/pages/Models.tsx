import { useEffect, useState, useCallback } from "react"
import {
  Pencil, Trash2, Loader2, Boxes,
  Zap, Plus, DollarSign, GripVertical, Unlink, LinkIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Card, CardContent } from "@/components/ui/card"
import { Badge } from "@/components/ui/badge"
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter,
} from "@/components/ui/dialog"
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select"
import {
  modelApi,
  type ModelInfo,
  type ModelBindingInfo,
  type UpdateModelData,
  type RoutingStrategy,
  type SmallModel,
} from "@/lib/api/models"
import { channelApi, type ChannelConfig } from "@/lib/api/channels"
import {
  pricingApi, emptyPricing,
  type ChannelModelPricing, type BillingType, type BindingPricingRow,
  BillingTypeToken, BillingTypePerCall, BillingTypeExpression,
} from "@/lib/api/pricing"
import { toast } from "@/hooks/use-toast"

// Index-circle colors decay from primary to muted as priority drops.
const INDEX_COLORS = [
  "bg-primary text-primary-foreground",
  "bg-primary/80 text-primary-foreground",
  "bg-primary/60 text-primary-foreground",
  "bg-muted text-muted-foreground",
  "bg-muted text-muted-foreground",
]

// Custom MIME type used to identify a drag originating from the small-model pool.
// (Reorder drags within a card do NOT set this type, so the card drop handler
// can distinguish "bind a new small model" from "reorder existing bindings".)
const POOL_DRAG_MIME = "application/x-chennix-small-model"

// dataTransfer.types is typed as DOMStringList but is a string[] at runtime.
const hasDragType = (e: React.DragEvent, mime: string): boolean => {
  const types = e.dataTransfer.types as unknown as string[]
  return types.includes(mime)
}

// Triple key: model_channels PK is (model_id, channel_id, upstream_model_name),
// so the same model can bind the same channel via multiple upstreams. All
// per-binding UI state must be keyed by the triple to avoid collisions.
const weightKey = (modelId: number, channelId: number, upstream: string) =>
  `${modelId}:${channelId}:${upstream}`

export default function Models() {
  const [models, setModels] = useState<ModelInfo[]>([])
  const [smallModels, setSmallModels] = useState<SmallModel[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")
  const [channels, setChannels] = useState<ChannelConfig[]>([])
  // Pricing rows keyed by `${modelId}:${channelId}:${upstream}` for pre-filling the edit dialog.
  const [pricingMap, setPricingMap] = useState<Record<string, BindingPricingRow>>({})

  // Edit (rename) dialog
  const [editDialogOpen, setEditDialogOpen] = useState(false)
  const [editingModel, setEditingModel] = useState<ModelInfo | null>(null)
  const [editName, setEditName] = useState("")
  const [saving, setSaving] = useState(false)

  // Create model dialog
  const [createDialogOpen, setCreateDialogOpen] = useState(false)
  const [createName, setCreateName] = useState("")
  const [creating, setCreating] = useState(false)

  // Delete confirmation
  const [deleteTarget, setDeleteTarget] = useState<ModelInfo | null>(null)
  const [deleting, setDeleting] = useState(false)

  // Pricing dialog
  const [pricingModel, setPricingModel] = useState<ModelInfo | null>(null)
  const [pricingBinding, setPricingBinding] = useState<ModelBindingInfo | null>(null)
  const [pricingForm, setPricingForm] = useState<ChannelModelPricing>(emptyPricing())
  const [pricingSaving, setPricingSaving] = useState(false)

  // Per-binding test loading state, keyed by `${modelId}:${channelId}:${upstream}`
  const [testStates, setTestStates] = useState<Record<string, { loading: boolean }>>({})

  // Reorder drag state: which binding is being dragged (within a model card, priority mode)
  const [dragging, setDragging] = useState<{ modelId: number; channelId: number } | null>(null)
  const [dragOver, setDragOver] = useState<{ modelId: number; channelId: number } | null>(null)

  // Pool → card drop highlight (which large-model card is currently a drop target)
  const [dropTargetModelId, setDropTargetModelId] = useState<number | null>(null)

  // LB weight input drafts, keyed by `${modelId}:${channelId}:${upstream}`
  const [weightDrafts, setWeightDrafts] = useState<Record<string, string>>({})
  const [weightSaving, setWeightSaving] = useState<Record<string, boolean>>({})

  // Per-model in-flight flags for binding add / strategy switch
  const [addingBinding, setAddingBinding] = useState<number | null>(null)
  const [switchingStrategy, setSwitchingStrategy] = useState<number | null>(null)

  const fetchModels = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const data = await modelApi.list()
      setModels(data)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "加载模型列表失败"
      setError(msg)
    } finally {
      setLoading(false)
    }
  }, [])

  const fetchSmallModels = useCallback(async () => {
    try {
      const data = await modelApi.listSmallModels()
      setSmallModels(data)
    } catch {
      // optional — small model pool is best-effort
    }
  }, [])

  const fetchChannels = useCallback(async () => {
    try {
      const data = await channelApi.list()
      setChannels(data)
    } catch {
      // optional — channels list is only used for small-model name lookup
    }
  }, [])

  const fetchPricing = useCallback(async () => {
    try {
      const rows = await pricingApi.list()
      const map: Record<string, BindingPricingRow> = {}
      for (const r of rows) {
        map[`${r.model_id}:${r.channel_id}:${r.upstream_model_name ?? ""}`] = r
      }
      setPricingMap(map)
    } catch {
      // optional — pricing is only used to pre-fill the edit dialog
    }
  }, [])

  useEffect(() => {
    fetchModels()
    fetchSmallModels()
    fetchChannels()
    fetchPricing()
  }, [fetchModels, fetchSmallModels, fetchChannels, fetchPricing])

  const refreshAll = useCallback(() => {
    fetchModels()
    fetchSmallModels()
    fetchPricing()
  }, [fetchModels, fetchSmallModels, fetchPricing])

  const channelName = (id: number) =>
    channels.find(c => c.id === id)?.name ?? `渠道 #${id}`

  // ===== Edit (rename) model =====
  const openEdit = (m: ModelInfo) => {
    setEditingModel(m)
    setEditName(m.canonical_name)
    setEditDialogOpen(true)
  }

  const handleSave = async () => {
    if (!editingModel) return
    if (!editName.trim()) {
      toast({ title: "请输入模型名称", variant: "destructive" })
      return
    }
    setSaving(true)
    try {
      const data: UpdateModelData = { canonical_name: editName.trim() }
      await modelApi.update(editingModel.id, data)
      toast({ title: "模型更新成功" })
      setEditDialogOpen(false)
      fetchModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setSaving(false)
    }
  }

  // ===== Create model =====
  const openCreate = () => {
    setCreateName("")
    setCreateDialogOpen(true)
  }

  const handleCreate = async () => {
    if (!createName.trim()) {
      toast({ title: "请输入模型名称", variant: "destructive" })
      return
    }
    setCreating(true)
    try {
      await modelApi.createModel(createName.trim())
      toast({ title: "大模型已创建" })
      setCreateDialogOpen(false)
      setCreateName("")
      fetchModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "创建失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setCreating(false)
    }
  }

  // ===== Delete model =====
  const handleDelete = async () => {
    if (!deleteTarget) return
    setDeleting(true)
    try {
      await modelApi.delete(deleteTarget.id)
      toast({ title: "模型已删除" })
      setDeleteTarget(null)
      refreshAll()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "删除失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setDeleting(false)
    }
  }

  // ===== Unbind (window.confirm) =====
  const handleUnbind = async (m: ModelInfo, b: ModelBindingInfo) => {
    const chanLabel = b.channel_name || `渠道 #${b.channel_id}`
    const ok = window.confirm(
      `确定要解除模型「${m.canonical_name}」与「${chanLabel} / ${b.upstream_model_name}」的绑定吗？` +
      (m.bindings.length <= 1 ? "\n解除后该模型将无可用渠道，无法处理请求。" : ""),
    )
    if (!ok) return
    try {
      await modelApi.removeBinding(m.id, b.channel_id, b.upstream_model_name)
      toast({ title: "绑定已解除" })
      refreshAll()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "解除绑定失败"
      toast({ title: msg, variant: "destructive" })
    }
  }

  // ===== Routing strategy =====
  const handleStrategyChange = async (m: ModelInfo, strategy: RoutingStrategy) => {
    if (m.routing_strategy === strategy) return
    setSwitchingStrategy(m.id)
    try {
      await modelApi.updateRoutingStrategy(m.id, strategy)
      toast({ title: "路由策略已切换" })
      fetchModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "切换策略失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setSwitchingStrategy(null)
    }
  }

  // ===== Weight update (load_balance mode) =====
  const commitWeight = async (m: ModelInfo, b: ModelBindingInfo) => {
    const key = weightKey(m.id, b.channel_id, b.upstream_model_name)
    const draft = weightDrafts[key] ?? String(b.weight)
    const num = parseInt(draft, 10)
    if (!Number.isFinite(num) || num < 1) {
      toast({ title: "权重必须为不小于 1 的整数", variant: "destructive" })
      setWeightDrafts(prev => {
        const next = { ...prev }
        delete next[key]
        return next
      })
      return
    }
    if (num === b.weight) {
      setWeightDrafts(prev => {
        const next = { ...prev }
        delete next[key]
        return next
      })
      return
    }
    setWeightSaving(prev => ({ ...prev, [key]: true }))
    try {
      await modelApi.updateBindingWeight(m.id, b.channel_id, b.upstream_model_name, num)
      toast({ title: "权重已更新" })
      setWeightDrafts(prev => {
        const next = { ...prev }
        delete next[key]
        return next
      })
      fetchModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "更新权重失败"
      toast({ title: msg, variant: "destructive" })
      setWeightDrafts(prev => ({ ...prev, [key]: String(b.weight) }))
    } finally {
      setWeightSaving(prev => {
        const next = { ...prev }
        delete next[key]
        return next
      })
    }
  }

  // ===== Pool drag → card drop (addBinding) =====
  const handlePoolDragStart = (e: React.DragEvent, s: SmallModel) => {
    e.dataTransfer.setData(
      POOL_DRAG_MIME,
      JSON.stringify({ channel_id: s.channel_id, raw_model_name: s.raw_model_name }),
    )
    e.dataTransfer.setData("text/plain", s.raw_model_name)
    e.dataTransfer.effectAllowed = "copy"
  }

  const handlePoolDragEnd = () => {
    setDropTargetModelId(null)
  }

  const handleCardDragOver = (e: React.DragEvent, m: ModelInfo) => {
    if (!hasDragType(e, POOL_DRAG_MIME)) return
    e.preventDefault()
    e.dataTransfer.dropEffect = "copy"
    if (dropTargetModelId !== m.id) setDropTargetModelId(m.id)
  }

  const handleCardDrop = async (e: React.DragEvent, m: ModelInfo) => {
    const payload = e.dataTransfer.getData(POOL_DRAG_MIME)
    if (!payload) return
    e.preventDefault()
    e.stopPropagation()
    setDropTargetModelId(null)
    let parsed: { channel_id: number; raw_model_name: string }
    try {
      parsed = JSON.parse(payload)
    } catch {
      return
    }
    // Client-side duplicate check for instant feedback; backend conflict also handled below.
    const dup = m.bindings.some(
      b => b.channel_id === parsed.channel_id && b.upstream_model_name === parsed.raw_model_name,
    )
    if (dup) {
      toast({ title: "该小模型已绑定到此大模型", variant: "destructive" })
      return
    }
    setAddingBinding(m.id)
    try {
      // upstream_model_name is taken from the dragged small model and is not editable.
      await modelApi.addBinding(m.id, parsed.channel_id, parsed.raw_model_name)
      toast({ title: "绑定添加成功" })
      refreshAll()
    } catch (err: unknown) {
      const resp = (err as { response?: { status?: number; data?: { message?: string } } })?.response
      const msg = resp?.data?.message || ""
      const status = resp?.status
      if (status === 409 || /already|conflict|duplicate|exists|已绑定|已存在/i.test(msg)) {
        toast({ title: "该小模型已绑定到此大模型", variant: "destructive" })
      } else {
        toast({ title: msg || "添加绑定失败", variant: "destructive" })
      }
    } finally {
      setAddingBinding(null)
    }
  }

  // ===== Reorder (priority mode, within a card) =====
  const handleRowDragStart = (e: React.DragEvent, modelId: number, channelId: number) => {
    e.dataTransfer.setData("text/plain", String(channelId))
    e.dataTransfer.effectAllowed = "move"
    setDragging({ modelId, channelId })
  }

  const handleRowDragOver = (e: React.DragEvent, modelId: number, channelId: number) => {
    // Always allow the drop on a row; if it's a reorder drag we handle it here,
    // otherwise (pool drag) we let it bubble to the card-level drop handler.
    e.preventDefault()
    if (dragging && dragging.modelId === modelId) {
      setDragOver({ modelId, channelId })
    }
  }

  const handleRowDrop = async (e: React.DragEvent, m: ModelInfo, targetChannelId: number) => {
    if (!dragging || dragging.modelId !== m.id) {
      // Not a reorder drag for this model — let the card-level handler deal with pool drags.
      return
    }
    e.preventDefault()
    e.stopPropagation()
    const draggedChannelId = dragging.channelId
    setDragging(null)
    setDragOver(null)
    if (draggedChannelId === targetChannelId) return
    const ordered = m.bindings.map(b => ({
      channel_id: b.channel_id,
      upstream_model_name: b.upstream_model_name,
    }))
    const fromIdx = ordered.findIndex(b => b.channel_id === draggedChannelId)
    const toIdx = ordered.findIndex(b => b.channel_id === targetChannelId)
    if (fromIdx === -1 || toIdx === -1) return
    const [moved] = ordered.splice(fromIdx, 1)
    ordered.splice(toIdx, 0, moved)
    try {
      await modelApi.reorder(m.id, ordered)
      toast({ title: "调用优先级已更新" })
      fetchModels()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "排序失败"
      toast({ title: msg, variant: "destructive" })
    }
  }

  const handleRowDragEnd = () => {
    setDragging(null)
    setDragOver(null)
    setDropTargetModelId(null)
  }

  // ===== Test binding =====
  const handleTestBinding = async (m: ModelInfo, b: ModelBindingInfo) => {
    const key = `${m.id}:${b.channel_id}:${b.upstream_model_name}`
    setTestStates(prev => ({ ...prev, [key]: { loading: true } }))
    try {
      const result = await modelApi.testBinding(m.id, b.channel_id, b.upstream_model_name)
      if (result.success) {
        toast({ title: `${b.channel_name} 连接成功`, description: `延迟: ${result.latency_ms}ms` })
      } else {
        toast({ title: `${b.channel_name} 连接失败`, description: result.error || "未知错误", variant: "destructive" })
      }
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "测试请求失败"
      toast({ title: `${b.channel_name} 测试失败`, description: msg, variant: "destructive" })
    } finally {
      setTestStates(prev => {
        const next = { ...prev }
        delete next[key]
        return next
      })
    }
  }

  // ===== Pricing dialog =====
  const openPricing = (m: ModelInfo, b: ModelBindingInfo) => {
    setPricingModel(m)
    setPricingBinding(b)
    const existing = pricingMap[`${m.id}:${b.channel_id}:${b.upstream_model_name}`]
    setPricingForm(existing?.pricing ?? emptyPricing())
  }

  const handleSavePricing = async () => {
    if (!pricingModel || !pricingBinding) return
    setPricingSaving(true)
    try {
      await pricingApi.update(pricingModel.id, {
        channel_id: pricingBinding.channel_id,
        upstream_model_name: pricingBinding.upstream_model_name,
        pricing: pricingForm,
      })
      toast({ title: "定价已保存" })
      setPricingModel(null)
      setPricingBinding(null)
      fetchPricing()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存定价失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setPricingSaving(false)
    }
  }

  const pricingSummary = (mId: number, cId: number, upstream: string): string | null => {
    const row = pricingMap[`${mId}:${cId}:${upstream}`]
    if (!row) return null
    const p = row.pricing
    if (p.billing_type === BillingTypeToken) return `¥${p.input_price}/¥${p.output_price} · 每1K`
    if (p.billing_type === BillingTypePerCall) return `¥${p.call_price}/次`
    if (p.billing_type === BillingTypeExpression) return `表达式计费`
    return null
  }

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
          <h1 className="text-2xl font-bold tracking-tight">模型管理</h1>
          <p className="text-sm text-muted-foreground mt-1">配置可用模型列表</p>
        </div>
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <p className="text-lg font-medium text-destructive">{error}</p>
            <Button variant="outline" className="mt-4" onClick={fetchModels}>重试</Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-bold tracking-tight">模型管理</h1>
          <p className="text-sm text-muted-foreground mt-1">
            从下方小模型池拖拽到大模型卡片完成绑定。优先级模式下可拖拽行排序，负载均衡模式下可设置权重。
          </p>
        </div>
        <Button onClick={openCreate} className="shrink-0">
          <Plus className="h-4 w-4 mr-1" />
          新建大模型
        </Button>
      </div>

      {/* Small model pool */}
      <Card>
        <CardContent className="p-4">
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-center gap-2">
              <Boxes className="h-4 w-4 text-muted-foreground" />
              <h2 className="text-sm font-semibold">小模型池</h2>
              <Badge variant="outline" className="shrink-0">{smallModels.length} 个</Badge>
            </div>
            <p className="text-xs text-muted-foreground hidden sm:block">拖拽下方任意小模型到大模型卡片即可绑定（支持一对多）</p>
          </div>
          {smallModels.length === 0 ? (
            <div className="flex items-center gap-2 rounded-md border border-dashed px-3 py-4 text-sm text-muted-foreground">
              <LinkIcon className="h-4 w-4 shrink-0" />
              <span>暂无已发现的小模型，请先在渠道管理中配置上游模型</span>
            </div>
          ) : (
            <div className="flex flex-wrap gap-2">
              {smallModels.map((s) => (
                <div
                  key={`${s.channel_id}:${s.raw_model_name}`}
                  draggable
                  onDragStart={(e) => handlePoolDragStart(e, s)}
                  onDragEnd={handlePoolDragEnd}
                  className="group flex items-center gap-2 rounded-md border bg-card px-2.5 py-2 cursor-grab hover:border-primary/50 hover:bg-accent transition-colors"
                  title={`拖拽到大模型卡片以绑定 · ${channelName(s.channel_id)}/${s.raw_model_name}`}
                >
                  <GripVertical className="h-3.5 w-3.5 text-muted-foreground shrink-0 group-hover:text-primary" />
                  <div className="min-w-0">
                    <div className="flex items-center gap-1.5">
                      <span className="font-mono text-xs font-medium truncate max-w-[180px]">{s.raw_model_name}</span>
                      <Badge variant="secondary" className="shrink-0 text-[10px] px-1.5 py-0 leading-none">
                        {channelName(s.channel_id)}
                      </Badge>
                    </div>
                    <span className="text-[10px] text-muted-foreground">已绑定 {s.binding_count} 个大模型</span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      {/* Large model cards */}
      {models.length === 0 ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <Boxes className="h-12 w-12 text-muted-foreground mb-4" />
            <p className="text-sm text-muted-foreground">暂无大模型，点击右上角「新建大模型」创建</p>
          </CardContent>
        </Card>
      ) : (
        <div className="grid grid-cols-1 lg:grid-cols-2 2xl:grid-cols-3 gap-4">
          {models.map((m) => {
            const isDropTarget = dropTargetModelId === m.id
            const isAdding = addingBinding === m.id
            const isSwitching = switchingStrategy === m.id
            const isPriority = m.routing_strategy === "priority"
            return (
              <Card
                key={m.id}
                onDragOver={(e) => handleCardDragOver(e, m)}
                onDrop={(e) => handleCardDrop(e, m)}
              >
                <CardContent className="p-4">
                  {/* Card header */}
                  <div className="flex items-start justify-between gap-2 mb-3">
                    <div className="flex items-center gap-2 min-w-0">
                      <span
                        className="font-semibold text-lg truncate cursor-text"
                        title="双击重命名"
                        onDoubleClick={() => openEdit(m)}
                      >
                        {m.canonical_name}
                      </span>
                      <Badge variant="outline" className="shrink-0">
                        {m.bindings.length} 个绑定
                      </Badge>
                    </div>
                    <div className="flex items-center gap-1 shrink-0">
                      <Button variant="ghost" size="icon" onClick={() => openEdit(m)} title="重命名" className="h-8 w-8">
                        <Pencil className="h-4 w-4" />
                      </Button>
                      <Button variant="ghost" size="icon" onClick={() => setDeleteTarget(m)} title="删除" className="h-8 w-8">
                        <Trash2 className="h-4 w-4 text-destructive" />
                      </Button>
                    </div>
                  </div>

                  {/* Routing strategy toggle */}
                  <div className="mb-3">
                    <div className="inline-flex rounded-md border p-0.5">
                      <button
                        type="button"
                        disabled={isSwitching}
                        onClick={() => handleStrategyChange(m, "priority")}
                        className={`px-2.5 py-1 text-xs rounded-sm transition-colors ${
                          isPriority
                            ? "bg-primary text-primary-foreground"
                            : "text-muted-foreground hover:text-foreground"
                        } ${isSwitching ? "opacity-50 cursor-not-allowed" : ""}`}
                      >
                        优先级
                      </button>
                      <button
                        type="button"
                        disabled={isSwitching}
                        onClick={() => handleStrategyChange(m, "load_balance")}
                        className={`px-2.5 py-1 text-xs rounded-sm transition-colors ${
                          !isPriority
                            ? "bg-primary text-primary-foreground"
                            : "text-muted-foreground hover:text-foreground"
                        } ${isSwitching ? "opacity-50 cursor-not-allowed" : ""}`}
                      >
                        负载均衡
                      </button>
                    </div>
                    <p className="text-[11px] text-muted-foreground mt-1">
                      {isPriority
                        ? "按优先级调用，拖拽行可调整顺序（序号越小越优先）"
                        : "按权重分配调用，设置每个绑定的权重（≥1）"}
                    </p>
                  </div>

                  {/* Bindings list / empty drop zone */}
                  {m.bindings.length === 0 ? (
                    <div
                      className={`flex items-center justify-center gap-2 rounded-md border border-dashed px-3 py-6 text-sm transition-colors ${
                        isDropTarget ? "border-primary bg-primary/5 text-primary" : "text-muted-foreground"
                      }`}
                    >
                      <LinkIcon className="h-4 w-4 shrink-0" />
                      <span>拖拽小模型到此绑定</span>
                    </div>
                  ) : (
                    <div className="space-y-1.5">
                      {m.bindings.map((b, idx) => {
                        const testKey = `${m.id}:${b.channel_id}:${b.upstream_model_name}`
                        const testState = testStates[testKey]
                        const wKey = weightKey(m.id, b.channel_id, b.upstream_model_name)
                        const isDragOverRow = dragOver?.modelId === m.id && dragOver?.channelId === b.channel_id
                        const isDraggingRow = dragging?.modelId === m.id && dragging?.channelId === b.channel_id
                        const summary = pricingSummary(m.id, b.channel_id, b.upstream_model_name)
                        return (
                          <div
                            key={`${b.channel_id}:${b.upstream_model_name}`}
                            draggable={isPriority}
                            onDragStart={(e) => isPriority && handleRowDragStart(e, m.id, b.channel_id)}
                            onDragOver={(e) => handleRowDragOver(e, m.id, b.channel_id)}
                            onDrop={(e) => handleRowDrop(e, m, b.channel_id)}
                            onDragEnd={handleRowDragEnd}
                            className={`flex items-center gap-2 rounded-md border px-2.5 py-2 transition-colors ${
                              isDragOverRow ? "border-primary bg-primary/5" : ""
                            } ${isDraggingRow ? "opacity-50" : ""} ${isPriority ? "cursor-grab" : ""}`}
                          >
                            {/* Drag handle (priority mode only) */}
                            {isPriority && (
                              <GripVertical className="h-4 w-4 text-muted-foreground cursor-grab shrink-0" />
                            )}

                            {/* Priority index circle */}
                            <span
                              className={`inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-medium ${
                                INDEX_COLORS[idx] ?? INDEX_COLORS[INDEX_COLORS.length - 1]
                              }`}
                              title={`优先级 ${b.priority}（越小越优先）`}
                            >
                              {idx + 1}
                            </span>

                            {/* Upstream model name + channel badge */}
                            <div className="flex flex-col min-w-0 flex-1 gap-0.5">
                              <div className="flex items-center gap-1.5 min-w-0">
                                <span className="font-mono text-xs font-medium truncate">
                                  {b.upstream_model_name}
                                </span>
                                <Badge variant="secondary" className="shrink-0 text-[10px] px-1.5 py-0 leading-none">
                                  {b.channel_name || `渠道 #${b.channel_id}`}
                                </Badge>
                              </div>
                              {summary && (
                                <span className="text-[10px] text-muted-foreground truncate">{summary}</span>
                              )}
                            </div>

                            {/* Weight input (load_balance mode only) */}
                            {!isPriority && (
                              <div className="flex items-center gap-1 shrink-0">
                                <Label className="text-[11px] text-muted-foreground">权重</Label>
                                <Input
                                  type="number"
                                  min={1}
                                  step={1}
                                  value={weightDrafts[wKey] ?? String(b.weight)}
                                  onChange={(e) => setWeightDrafts(prev => ({ ...prev, [wKey]: e.target.value }))}
                                  onBlur={() => commitWeight(m, b)}
                                  disabled={weightSaving[wKey]}
                                  className="h-7 w-16"
                                  title="权重（≥1）"
                                />
                              </div>
                            )}

                            {/* Row actions */}
                            <div className="flex items-center gap-1 shrink-0">
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => handleTestBinding(m, b)}
                                disabled={testState?.loading}
                                title="测试该绑定连通性"
                                className="h-8 px-2"
                              >
                                {testState?.loading ? (
                                  <Loader2 className="h-4 w-4 animate-spin" />
                                ) : (
                                  <Zap className="h-4 w-4" />
                                )}
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => openPricing(m, b)}
                                title="编辑定价"
                                className="h-8 px-2"
                              >
                                <DollarSign className="h-4 w-4" />
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => handleUnbind(m, b)}
                                title="解除绑定"
                                className="h-8 px-2"
                              >
                                <Unlink className="h-4 w-4 text-destructive" />
                              </Button>
                            </div>
                          </div>
                        )
                      })}
                    </div>
                  )}

                  {/* Drop target hint / adding indicator */}
                  {(isDropTarget || isAdding) && (
                    <div
                      className={`mt-2 flex items-center justify-center gap-2 rounded-md border border-dashed px-3 py-2 text-xs transition-colors ${
                        isDropTarget ? "border-primary bg-primary/5 text-primary" : "text-muted-foreground"
                      }`}
                    >
                      {isAdding ? (
                        <>
                          <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          <span>正在绑定...</span>
                        </>
                      ) : (
                        <span>松开以绑定到此大模型</span>
                      )}
                    </div>
                  )}
                </CardContent>
              </Card>
            )
          })}
        </div>
      )}

      {/* Create model dialog */}
      <Dialog open={createDialogOpen} onOpenChange={setCreateDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>新建大模型</DialogTitle>
            <DialogDescription>创建一个新的模型条目，随后可拖拽小模型进行绑定</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="c-name">模型名称</Label>
            <Input
              id="c-name"
              value={createName}
              onChange={(e) => setCreateName(e.target.value)}
              placeholder="如 gpt-4o"
              onKeyDown={(e) => e.key === "Enter" && handleCreate()}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateDialogOpen(false)}>取消</Button>
            <Button onClick={handleCreate} disabled={creating}>
              {creating && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {creating ? "创建中..." : "创建"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Edit (rename) dialog */}
      <Dialog open={editDialogOpen} onOpenChange={setEditDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>编辑模型</DialogTitle>
            <DialogDescription>修改模型的规范名称</DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="m-name">模型名称</Label>
            <Input
              id="m-name"
              value={editName}
              onChange={(e) => setEditName(e.target.value)}
              placeholder="如 gpt-4o"
              onKeyDown={(e) => e.key === "Enter" && handleSave()}
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setEditDialogOpen(false)}>取消</Button>
            <Button onClick={handleSave} disabled={saving}>
              {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {saving ? "保存中..." : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete confirmation */}
      <Dialog open={!!deleteTarget} onOpenChange={(o) => !o && setDeleteTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除模型</DialogTitle>
            <DialogDescription>
              确定要删除模型「{deleteTarget?.canonical_name}」吗？此操作不可撤销，相关的渠道绑定也将被移除。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>取消</Button>
            <Button variant="destructive" onClick={handleDelete} disabled={deleting}>
              {deleting && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {deleting ? "删除中..." : "删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Pricing edit dialog */}
      <Dialog open={!!pricingModel} onOpenChange={(o) => !o && setPricingModel(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>编辑定价</DialogTitle>
            <DialogDescription>
              {pricingModel?.canonical_name} · {pricingBinding?.channel_name}
              {pricingBinding && pricingBinding.upstream_model_name !== pricingModel?.canonical_name
                ? ` → ${pricingBinding.upstream_model_name}`
                : ""}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label>计费模式</Label>
              <Select
                value={pricingForm.billing_type}
                onValueChange={(v) => setPricingForm(f => ({
                  ...f,
                  billing_type: v as BillingType,
                }))}
              >
                <SelectTrigger><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value={BillingTypeToken}>按 Token（元/1K tokens）</SelectItem>
                  <SelectItem value={BillingTypePerCall}>按调用次数（元/次）</SelectItem>
                  <SelectItem value={BillingTypeExpression}>分段表达式</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {pricingForm.billing_type === BillingTypeToken && (
              <div className="grid grid-cols-2 gap-3">
                <div className="space-y-2">
                  <Label htmlFor="p-input">输入价格（元/1K tokens）</Label>
                  <Input
                    id="p-input"
                    type="number"
                    step="0.0001"
                    min="0"
                    value={pricingForm.input_price}
                    onChange={(e) => setPricingForm(f => ({ ...f, input_price: parseFloat(e.target.value) || 0 }))}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="p-output">输出价格（元/1K tokens）</Label>
                  <Input
                    id="p-output"
                    type="number"
                    step="0.0001"
                    min="0"
                    value={pricingForm.output_price}
                    onChange={(e) => setPricingForm(f => ({ ...f, output_price: parseFloat(e.target.value) || 0 }))}
                  />
                </div>
                <p className="col-span-2 text-xs text-muted-foreground">
                  费用 = (输入 tokens / 1000 × 输入价格) + (输出 tokens / 1000 × 输出价格)
                </p>
              </div>
            )}

            {pricingForm.billing_type === BillingTypePerCall && (
              <div className="space-y-2">
                <Label htmlFor="p-call">每次调用费用（元/次）</Label>
                <Input
                  id="p-call"
                  type="number"
                  step="0.0001"
                  min="0"
                  value={pricingForm.call_price}
                  onChange={(e) => setPricingForm(f => ({ ...f, call_price: parseFloat(e.target.value) || 0 }))}
                />
                <p className="text-xs text-muted-foreground">无论 token 数量，每次调用固定收取此费用。</p>
              </div>
            )}

            {pricingForm.billing_type === BillingTypeExpression && (
              <div className="space-y-2">
                <Label htmlFor="p-expr">计费表达式</Label>
                <Input
                  id="p-expr"
                  className="font-mono text-xs"
                  value={pricingForm.billing_expr ?? ""}
                  onChange={(e) => setPricingForm(f => ({ ...f, billing_expr: e.target.value || null }))}
                  placeholder="如：if(total > 10000, p * 0.0000005 + c * 0.000001, p * 0.000001 + c * 0.000002)"
                />
                <div className="rounded-md bg-muted/50 p-2.5 text-xs text-muted-foreground space-y-1">
                  <p>可用变量：</p>
                  <p className="font-mono pl-2">p — 输入 tokens</p>
                  <p className="font-mono pl-2">c — 输出 tokens</p>
                  <p className="font-mono pl-2">total — 总 tokens (p + c)</p>
                  <p className="pt-1">示例：</p>
                  <p className="font-mono pl-2 break-all">p / 1000 * 0.001 + c / 1000 * 0.002</p>
                  <p className="font-mono pl-2 break-all">if(total &gt; 10000, 5, 10)</p>
                  <p className="font-mono pl-2 break-all">1.5</p>
                  <p className="pt-1">表达式结果单位为「元」，支持 + - * / % 比较运算与 if(cond, then, else) 函数。</p>
                </div>
              </div>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setPricingModel(null)}>取消</Button>
            <Button onClick={handleSavePricing} disabled={pricingSaving}>
              {pricingSaving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {pricingSaving ? "保存中..." : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

import { useEffect, useState, useCallback } from "react"
import {
  Loader2, Pencil, Boxes, RefreshCw,
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
  Table, TableHeader, TableBody, TableHead, TableRow, TableCell,
} from "@/components/ui/table"
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select"
import {
  pricingApi, emptyPricing,
  type BindingPricingRow, type ChannelModelPricing, type BillingType,
  BillingTypeToken, BillingTypePerCall, BillingTypeExpression,
} from "@/lib/api/pricing"
import { toast } from "@/hooks/use-toast"

function billingTypeBadge(bt: BillingType): { label: string; variant: "default" | "secondary" | "outline" } {
  switch (bt) {
    case BillingTypeToken: return { label: "按 Token", variant: "default" }
    case BillingTypePerCall: return { label: "按调用次数", variant: "secondary" }
    case BillingTypeExpression: return { label: "分段表达式", variant: "outline" }
    default: return { label: String(bt), variant: "outline" as const }
  }
}

function pricingSummary(p: ChannelModelPricing): string {
  switch (p.billing_type) {
    case BillingTypeToken:
      if (p.input_price === 0 && p.output_price === 0) return "未配置"
      return `${p.input_price} / ${p.output_price} 元/1K`
    case BillingTypePerCall:
      return p.call_price > 0 ? `${p.call_price} 元/次` : "未配置"
    case BillingTypeExpression:
      return p.billing_expr && p.billing_expr.trim() ? p.billing_expr : "未配置"
    default:
      return "未配置"
  }
}

export default function Pricing() {
  const [rows, setRows] = useState<BindingPricingRow[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")

  // Edit dialog state
  const [editOpen, setEditOpen] = useState(false)
  const [editRow, setEditRow] = useState<BindingPricingRow | null>(null)
  const [editForm, setEditForm] = useState<ChannelModelPricing>(emptyPricing())
  const [saving, setSaving] = useState(false)

  const fetchRows = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const data = await pricingApi.list()
      setRows(data)
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "加载定价列表失败"
      setError(msg)
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchRows()
  }, [fetchRows])

  const openEdit = (row: BindingPricingRow) => {
    setEditRow(row)
    setEditForm({ ...row.pricing })
    setEditOpen(true)
  }

  const handleSave = async () => {
    if (!editRow) return
    setSaving(true)
    try {
      await pricingApi.update(editRow.model_id, {
        channel_id: editRow.channel_id,
        upstream_model_name: editRow.upstream_model_name ?? '',
        pricing: editForm,
      })
      toast({ title: "定价已保存" })
      setEditOpen(false)
      fetchRows()
    } catch (err: unknown) {
      const msg = (err as { response?: { data?: { message?: string } } })?.response?.data?.message || "保存失败"
      toast({ title: msg, variant: "destructive" })
    } finally {
      setSaving(false)
    }
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
          <h1 className="text-2xl font-bold tracking-tight">定价管理</h1>
          <p className="text-sm text-muted-foreground mt-1">查看所有渠道-模型绑定的定价配置</p>
        </div>
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <p className="text-lg font-medium text-destructive">{error}</p>
            <Button variant="outline" className="mt-4" onClick={fetchRows}>重试</Button>
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
          <h1 className="text-2xl font-bold tracking-tight">定价管理</h1>
          <p className="text-sm text-muted-foreground mt-1">
            管理每个渠道-模型绑定的定价（同一模型在不同渠道可有不同价格）
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={fetchRows}>
          <RefreshCw className="mr-2 h-4 w-4" />
          刷新
        </Button>
      </div>

      {/* Pricing Table */}
      <Card>
        <CardContent className="p-0">
          {rows.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-16">
              <Boxes className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-sm text-muted-foreground">暂无定价配置，请先在渠道管理中添加模型</p>
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>标准模型</TableHead>
                  <TableHead>渠道</TableHead>
                  <TableHead>上游模型名</TableHead>
                  <TableHead>计费模式</TableHead>
                  <TableHead>定价</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((r) => {
                  const badge = billingTypeBadge(r.pricing.billing_type)
                  const summary = pricingSummary(r.pricing)
                  const unconfigured = summary === "未配置"
                  return (
                    <TableRow key={`${r.model_id}-${r.channel_id}`}>
                      <TableCell className="font-semibold">{r.canonical_name}</TableCell>
                      <TableCell>
                        <Badge variant="outline" className="font-normal">{r.channel_name}</Badge>
                      </TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">
                        {r.upstream_model_name || "—"}
                      </TableCell>
                      <TableCell>
                        <Badge variant={badge.variant}>{badge.label}</Badge>
                      </TableCell>
                      <TableCell className={unconfigured ? "text-muted-foreground" : ""}>
                        {summary}
                      </TableCell>
                      <TableCell className="text-right">
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => openEdit(r)}
                          title="编辑定价"
                          className="h-8 px-2"
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      {/* Edit Dialog */}
      <Dialog open={editOpen} onOpenChange={setEditOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>编辑定价</DialogTitle>
            <DialogDescription>
              {editRow?.canonical_name} · {editRow?.channel_name}
              {editRow?.upstream_model_name && editRow.upstream_model_name !== editRow.canonical_name
                ? ` → ${editRow.upstream_model_name}`
                : ""}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label>计费模式</Label>
              <Select
                value={editForm.billing_type}
                onValueChange={(v) => setEditForm(f => ({
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

            {editForm.billing_type === BillingTypeToken && (
              <div className="grid grid-cols-2 gap-3">
                <div className="space-y-2">
                  <Label htmlFor="p-input">输入价格（元/1K tokens）</Label>
                  <Input
                    id="p-input"
                    type="number"
                    step="0.0001"
                    min="0"
                    value={editForm.input_price}
                    onChange={(e) => setEditForm(f => ({ ...f, input_price: parseFloat(e.target.value) || 0 }))}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="p-output">输出价格（元/1K tokens）</Label>
                  <Input
                    id="p-output"
                    type="number"
                    step="0.0001"
                    min="0"
                    value={editForm.output_price}
                    onChange={(e) => setEditForm(f => ({ ...f, output_price: parseFloat(e.target.value) || 0 }))}
                  />
                </div>
                <p className="col-span-2 text-xs text-muted-foreground">
                  费用 = (输入 tokens / 1000 × 输入价格) + (输出 tokens / 1000 × 输出价格)
                </p>
              </div>
            )}

            {editForm.billing_type === BillingTypePerCall && (
              <div className="space-y-2">
                <Label htmlFor="p-call">每次调用费用（元/次）</Label>
                <Input
                  id="p-call"
                  type="number"
                  step="0.0001"
                  min="0"
                  value={editForm.call_price}
                  onChange={(e) => setEditForm(f => ({ ...f, call_price: parseFloat(e.target.value) || 0 }))}
                />
                <p className="text-xs text-muted-foreground">无论 token 数量，每次调用固定收取此费用。</p>
              </div>
            )}

            {editForm.billing_type === BillingTypeExpression && (
              <div className="space-y-2">
                <Label htmlFor="p-expr">计费表达式</Label>
                <Input
                  id="p-expr"
                  className="font-mono text-xs"
                  value={editForm.billing_expr ?? ""}
                  onChange={(e) => setEditForm(f => ({ ...f, billing_expr: e.target.value || null }))}
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
            <Button variant="outline" onClick={() => setEditOpen(false)}>取消</Button>
            <Button onClick={handleSave} disabled={saving}>
              {saving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              {saving ? "保存中..." : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

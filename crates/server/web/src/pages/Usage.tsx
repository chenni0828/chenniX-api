import { useState, useEffect, useCallback } from "react"
import {
  BarChart3, Download, Loader2, Search, RefreshCw,
} from "lucide-react"
import {
  Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis,
} from "recharts"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow, TableFooter,
} from "@/components/ui/table"
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select"
import { usageApi, type UsageSummary } from "@/lib/api/usage"
import { channelApi, type ChannelConfig } from "@/lib/api/channels"
import { formatNumber, formatCost, getErrorMessage, daysAgoTimestamp } from "@/lib/format"

const TIME_RANGES = [
  { value: "7", label: "最近 7 天" },
  { value: "30", label: "最近 30 天" },
  { value: "custom", label: "自定义" },
]

function exportCSV(data: UsageSummary[]) {
  const headers = ["渠道名称", "模型", "总Token数", "请求数", "总消耗"]
  const rows = data.map((d) => [d.channel_name, d.model, d.total_tokens, d.request_count, d.total_cost])
  const csv = [headers, ...rows]
    .map((r) => r.map((c) => `"${c}"`).join(","))
    .join("\n")
  const blob = new Blob(["\ufeff" + csv], { type: "text/csv;charset=utf-8;" })
  const url = URL.createObjectURL(blob)
  const a = document.createElement("a")
  a.href = url
  a.download = `usage-${new Date().toISOString().slice(0, 10)}.csv`
  a.click()
  URL.revokeObjectURL(url)
}

export default function Usage() {
  const [data, setData] = useState<UsageSummary[]>([])
  const [channels, setChannels] = useState<ChannelConfig[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState("")

  // Filters
  const [channelId, setChannelId] = useState<string>("all")
  const [model, setModel] = useState("")
  const [timeRange, setTimeRange] = useState("7")
  const [customStart, setCustomStart] = useState("")
  const [customEnd, setCustomEnd] = useState("")

  const fetchChannels = useCallback(async () => {
    try {
      const data = await channelApi.list()
      setChannels(data)
    } catch {
      // optional — channel filter still works with manual input
    }
  }, [])

  useEffect(() => {
    fetchChannels()
  }, [fetchChannels])

  const buildQuery = useCallback(() => {
    const params: Record<string, string | number> = {}
    if (channelId !== "all") params.channel_id = parseInt(channelId)
    if (model.trim()) params.model = model.trim()
    if (timeRange === "custom") {
      if (customStart) params.start = datetimeToTimestamp(customStart)
      if (customEnd) params.end = datetimeToTimestamp(customEnd, true)
    } else {
      const days = parseInt(timeRange)
      params.start = daysAgoTimestamp(days)
    }
    return params
  }, [channelId, model, timeRange, customStart, customEnd])

  const fetchData = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const result = await usageApi.get(buildQuery())
      setData(result)
    } catch (err) {
      setError(getErrorMessage(err, "加载用量数据失败"))
    } finally {
      setLoading(false)
    }
  }, [buildQuery])

  useEffect(() => {
    fetchData()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Chart data: aggregate by channel
  const chartData = data.reduce<{ name: string; tokens: number; cost: number }[]>((acc, item) => {
    const existing = acc.find((x) => x.name === item.channel_name)
    if (existing) {
      existing.tokens += item.total_tokens
      existing.cost += item.total_cost
    } else {
      acc.push({ name: item.channel_name, tokens: item.total_tokens, cost: item.total_cost })
    }
    return acc
  }, [])

  const totals = data.reduce(
    (acc, d) => ({
      total_tokens: acc.total_tokens + d.total_tokens,
      request_count: acc.request_count + d.request_count,
      total_cost: acc.total_cost + d.total_cost,
    }),
    { total_tokens: 0, request_count: 0, total_cost: 0 }
  )

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">用量统计</h1>
          <p className="mt-1 text-sm text-muted-foreground">查看 API 调用量和配额使用情况</p>
        </div>
        <Button variant="outline" size="sm" onClick={() => exportCSV(data)} disabled={data.length === 0}>
          <Download className="h-4 w-4" />
          导出 CSV
        </Button>
      </div>

      {/* Filters */}
      <Card>
        <CardContent className="flex flex-wrap items-end gap-4 pt-6">
          <div className="space-y-2">
            <Label className="text-xs text-muted-foreground">渠道</Label>
            <Select value={channelId} onValueChange={setChannelId}>
              <SelectTrigger className="w-44"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部渠道</SelectItem>
                {channels.map((c) => (
                  <SelectItem key={c.id} value={String(c.id)}>{c.name}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-2">
            <Label className="text-xs text-muted-foreground">模型</Label>
            <Input
              className="w-44"
              placeholder="模型名称"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && fetchData()}
            />
          </div>
          <div className="space-y-2">
            <Label className="text-xs text-muted-foreground">时间范围</Label>
            <Select value={timeRange} onValueChange={setTimeRange}>
              <SelectTrigger className="w-36"><SelectValue /></SelectTrigger>
              <SelectContent>
                {TIME_RANGES.map((r) => (
                  <SelectItem key={r.value} value={r.value}>{r.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {timeRange === "custom" && (
            <>
              <div className="space-y-2">
                <Label className="text-xs text-muted-foreground">开始日期</Label>
                <Input
                  type="date"
                  className="w-40"
                  value={customStart}
                  onChange={(e) => setCustomStart(e.target.value)}
                />
              </div>
              <div className="space-y-2">
                <Label className="text-xs text-muted-foreground">结束日期</Label>
                <Input
                  type="date"
                  className="w-40"
                  value={customEnd}
                  onChange={(e) => setCustomEnd(e.target.value)}
                />
              </div>
            </>
          )}
          <Button size="sm" onClick={fetchData} disabled={loading}>
            {loading ? <Loader2 className="h-4 w-4 animate-spin" /> : <Search className="h-4 w-4" />}
            查询
          </Button>
          <Button variant="ghost" size="sm" onClick={fetchData} disabled={loading}>
            <RefreshCw className="h-4 w-4" />
          </Button>
        </CardContent>
      </Card>

      {/* Error */}
      {error && (
        <div className="rounded-md bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</div>
      )}

      {/* Chart */}
      {!loading && data.length > 0 && chartData.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <BarChart3 className="h-5 w-5 text-muted-foreground" />
              渠道 Token 消耗
            </CardTitle>
          </CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={300}>
              <BarChart data={chartData} margin={{ top: 8, right: 16, left: 0, bottom: 8 }}>
                <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
                <XAxis
                  dataKey="name"
                  tick={{ fontSize: 12 }}
                  className="fill-muted-foreground"
                  interval={0}
                  angle={-15}
                  textAnchor="end"
                  height={60}
                />
                <YAxis tick={{ fontSize: 12 }} className="fill-muted-foreground" />
                <Tooltip
                  contentStyle={{
                    backgroundColor: "var(--color-popover)",
                    border: "1px solid var(--color-border)",
                    borderRadius: "0.5rem",
                    fontSize: "12px",
                  }}
                  formatter={(value: number) => [formatNumber(value), "Token 数"]}
                />
                <Bar dataKey="tokens" fill="hsl(221.2 83.2% 53.3%)" radius={[4, 4, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>
      )}

      {/* Table */}
      <Card>
        <CardContent className="p-0">
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : data.length === 0 ? (
            <div className="flex items-center justify-center py-16 text-sm text-muted-foreground">
              暂无用量数据
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>渠道名称</TableHead>
                  <TableHead>模型</TableHead>
                  <TableHead className="text-right">总 Token 数</TableHead>
                  <TableHead className="text-right">请求数</TableHead>
                  <TableHead className="text-right">总消耗</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {data.map((d, i) => (
                  <TableRow key={i}>
                    <TableCell className="font-medium">{d.channel_name}</TableCell>
                    <TableCell>
                      <code className="rounded bg-muted px-1.5 py-0.5 text-xs">{d.model}</code>
                    </TableCell>
                    <TableCell className="text-right tabular-nums">{formatNumber(d.total_tokens)}</TableCell>
                    <TableCell className="text-right tabular-nums">{formatNumber(d.request_count)}</TableCell>
                    <TableCell className="text-right tabular-nums">{formatCost(d.total_cost)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
              <TableFooter>
                <TableRow>
                  <TableCell colSpan={2} className="font-semibold">总计</TableCell>
                  <TableCell className="text-right tabular-nums font-semibold">{formatNumber(totals.total_tokens)}</TableCell>
                  <TableCell className="text-right tabular-nums font-semibold">{formatNumber(totals.request_count)}</TableCell>
                  <TableCell className="text-right tabular-nums font-semibold">{formatCost(totals.total_cost)}</TableCell>
                </TableRow>
              </TableFooter>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

/** Convert a date input (YYYY-MM-DD) to a Unix timestamp (start or end of day). */
function datetimeToTimestamp(dateStr: string, endOfDay = false): number {
  if (!dateStr) return 0
  const d = new Date(dateStr)
  if (isNaN(d.getTime())) return 0
  if (endOfDay) {
    d.setHours(23, 59, 59)
  } else {
    d.setHours(0, 0, 0)
  }
  return Math.floor(d.getTime() / 1000)
}

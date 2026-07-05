import { useState, useEffect, useCallback } from "react"
import {
  Loader2, Search, RefreshCw, ChevronLeft, ChevronRight,
  ChevronsLeft, ChevronsRight,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent } from "@/components/ui/card"
import {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from "@/components/ui/table"
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select"
import { logApi, type RequestLog, type LogsQuery } from "@/lib/api/logs"
import { channelApi, type ChannelConfig } from "@/lib/api/channels"
import { formatNumber, formatCost, formatDate, getErrorMessage, daysAgoTimestamp } from "@/lib/format"

const PER_PAGE = 20
const TIME_RANGES = [
  { value: "0", label: "全部时间" },
  { value: "7", label: "最近 7 天" },
  { value: "30", label: "最近 30 天" },
  { value: "custom", label: "自定义" },
]

function statusBadge(code: number) {
  if (code >= 200 && code < 300) return <Badge variant="success">{code}</Badge>
  if (code >= 400 && code < 500) return <Badge variant="warning">{code}</Badge>
  if (code >= 500) return <Badge variant="destructive">{code}</Badge>
  return <Badge variant="secondary">{code}</Badge>
}

export default function Logs() {
  const [logs, setLogs] = useState<RequestLog[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [channels, setChannels] = useState<ChannelConfig[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState("")

  // Filters
  const [channelId, setChannelId] = useState<string>("all")
  const [model, setModel] = useState("")
  const [statusCode, setStatusCode] = useState("")
  const [timeRange, setTimeRange] = useState("0")
  const [customStart, setCustomStart] = useState("")
  const [customEnd, setCustomEnd] = useState("")

  const totalPages = Math.max(1, Math.ceil(total / PER_PAGE))

  const fetchChannels = useCallback(async () => {
    try {
      const data = await channelApi.list()
      setChannels(data)
    } catch {
      // optional
    }
  }, [])

  useEffect(() => {
    fetchChannels()
  }, [fetchChannels])

  const buildQuery = useCallback((): LogsQuery => {
    const params: LogsQuery = { page, per_page: PER_PAGE }
    if (channelId !== "all") params.channel_id = parseInt(channelId)
    if (model.trim()) params.model = model.trim()
    if (statusCode.trim()) params.status_code = parseInt(statusCode)
    if (timeRange === "custom") {
      if (customStart) params.start = dateToTimestamp(customStart)
      if (customEnd) params.end = dateToTimestamp(customEnd, true)
    } else if (timeRange !== "0") {
      params.start = daysAgoTimestamp(parseInt(timeRange))
    }
    return params
  }, [page, channelId, model, statusCode, timeRange, customStart, customEnd])

  const fetchLogs = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const res = await logApi.get(buildQuery())
      setLogs(res.logs)
      setTotal(res.total)
    } catch (err) {
      setError(getErrorMessage(err, "加载日志失败"))
    } finally {
      setLoading(false)
    }
  }, [buildQuery])

  useEffect(() => {
    fetchLogs()
  }, [fetchLogs])

  const handleSearch = () => {
    setPage(1)
    fetchLogs()
  }

  const goToPage = (p: number) => {
    setPage(Math.min(Math.max(1, p), totalPages))
  }

  // Render page numbers with ellipsis
  const renderPageNumbers = () => {
    const pages: (number | string)[] = []
    const showAround = 1 // pages to show around current
    for (let i = 1; i <= totalPages; i++) {
      if (i === 1 || i === totalPages || (i >= page - showAround && i <= page + showAround)) {
        pages.push(i)
      } else if (pages[pages.length - 1] !== "...") {
        pages.push("...")
      }
    }
    return pages
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-2xl font-bold tracking-tight">请求日志</h1>
        <p className="mt-1 text-sm text-muted-foreground">查看 API 请求历史记录</p>
      </div>

      {/* Filters */}
      <Card>
        <CardContent className="flex flex-wrap items-end gap-4 pt-6">
          <div className="space-y-2">
            <Label className="text-xs text-muted-foreground">渠道</Label>
            <Select value={channelId} onValueChange={setChannelId}>
              <SelectTrigger className="w-40"><SelectValue /></SelectTrigger>
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
              className="w-36"
              placeholder="模型名称"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSearch()}
            />
          </div>
          <div className="space-y-2">
            <Label className="text-xs text-muted-foreground">状态码</Label>
            <Input
              className="w-24"
              type="number"
              placeholder="如 200"
              value={statusCode}
              onChange={(e) => setStatusCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSearch()}
            />
          </div>
          <div className="space-y-2">
            <Label className="text-xs text-muted-foreground">时间范围</Label>
            <Select value={timeRange} onValueChange={setTimeRange}>
              <SelectTrigger className="w-32"><SelectValue /></SelectTrigger>
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
                <Label className="text-xs text-muted-foreground">开始</Label>
                <Input type="date" className="w-36" value={customStart}
                  onChange={(e) => setCustomStart(e.target.value)} />
              </div>
              <div className="space-y-2">
                <Label className="text-xs text-muted-foreground">结束</Label>
                <Input type="date" className="w-36" value={customEnd}
                  onChange={(e) => setCustomEnd(e.target.value)} />
              </div>
            </>
          )}
          <Button size="sm" onClick={handleSearch} disabled={loading}>
            {loading ? <Loader2 className="h-4 w-4 animate-spin" /> : <Search className="h-4 w-4" />}
            查询
          </Button>
          <Button variant="ghost" size="sm" onClick={fetchLogs} disabled={loading}>
            <RefreshCw className="h-4 w-4" />
          </Button>
        </CardContent>
      </Card>

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
          ) : logs.length === 0 ? (
            <div className="flex items-center justify-center py-16 text-sm text-muted-foreground">
              暂无日志数据
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="min-w-[150px]">时间</TableHead>
                  <TableHead className="text-right">用户</TableHead>
                  <TableHead className="text-right">Token</TableHead>
                  <TableHead>渠道</TableHead>
                  <TableHead className="min-w-[160px]">模型</TableHead>
                  <TableHead>状态码</TableHead>
                  <TableHead className="text-right">消耗</TableHead>
                  <TableHead className="text-right">耗时</TableHead>
                  <TableHead className="min-w-[200px]">错误信息</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {logs.map((log) => {
                  // 客户端请求的模型名（归一化后的大模型）
                  const clientModel = log.normalized_model || log.client_model
                  // 实际命中的上游模型（绑定时配置的 upstream_model_name）
                  const upstreamModel = log.upstream_model
                  // 客户端模型与上游模型相同（或无上游模型）时只显示一行
                  const sameModel = !upstreamModel || upstreamModel === clientModel
                  return (
                    <TableRow key={log.id}>
                      <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                        {formatDate(log.created_at)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums text-xs">
                        {log.user_id ?? "-"}
                      </TableCell>
                      <TableCell className="text-right tabular-nums text-xs">
                        {log.token_id ?? "-"}
                      </TableCell>
                      <TableCell className="text-xs">{log.channel_name ?? "-"}</TableCell>
                      <TableCell className="text-xs">
                        {sameModel ? (
                          <span>{clientModel ?? "-"}</span>
                        ) : (
                          <div className="flex flex-col gap-0.5">
                            <span>{clientModel ?? "-"}</span>
                            <span className="text-[10px] text-muted-foreground" title="实际调用的上游模型">
                              → {upstreamModel}
                            </span>
                          </div>
                        )}
                      </TableCell>
                      <TableCell>{statusBadge(log.response_status)}</TableCell>
                      <TableCell className="text-right tabular-nums text-xs">
                        {formatCost(log.quota_cost)}
                      </TableCell>
                      <TableCell className="text-right tabular-nums text-xs">
                        {formatNumber(log.duration_ms)} ms
                      </TableCell>
                      <TableCell className="max-w-[300px] truncate text-xs text-muted-foreground" title={log.error_message ?? ""}>
                        {log.error_message ?? "-"}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>

      {/* Pagination */}
      {!loading && total > 0 && (
        <div className="flex items-center justify-between">
          <p className="text-sm text-muted-foreground">
            共 <span className="font-medium text-foreground">{formatNumber(total)}</span> 条记录，
           第 {page} / {totalPages} 页
          </p>
          <div className="flex items-center gap-1">
            <Button variant="outline" size="icon" className="h-8 w-8"
              onClick={() => goToPage(1)} disabled={page <= 1}>
              <ChevronsLeft className="h-4 w-4" />
            </Button>
            <Button variant="outline" size="icon" className="h-8 w-8"
              onClick={() => goToPage(page - 1)} disabled={page <= 1}>
              <ChevronLeft className="h-4 w-4" />
            </Button>
            {renderPageNumbers().map((p, i) =>
              p === "..." ? (
                <span key={`e-${i}`} className="px-2 text-muted-foreground">...</span>
              ) : (
                <Button
                  key={p}
                  variant={p === page ? "default" : "outline"}
                  size="icon"
                  className="h-8 w-8"
                  onClick={() => goToPage(p as number)}
                >
                  {p}
                </Button>
              )
            )}
            <Button variant="outline" size="icon" className="h-8 w-8"
              onClick={() => goToPage(page + 1)} disabled={page >= totalPages}>
              <ChevronRight className="h-4 w-4" />
            </Button>
            <Button variant="outline" size="icon" className="h-8 w-8"
              onClick={() => goToPage(totalPages)} disabled={page >= totalPages}>
              <ChevronsRight className="h-4 w-4" />
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}

/** Convert a date input (YYYY-MM-DD) to a Unix timestamp (start or end of day). */
function dateToTimestamp(dateStr: string, endOfDay = false): number {
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

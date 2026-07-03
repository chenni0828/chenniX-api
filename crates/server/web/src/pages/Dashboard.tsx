import { useEffect, useState, useCallback } from "react"
import {
  Activity, Send, AlertCircle, KeyRound, RefreshCw, TrendingUp, PieChart as PieChartIcon,
} from "lucide-react"
import {
  ResponsiveContainer, BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip,
  PieChart, Pie, Cell,
} from "recharts"
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import {
  Table, TableHeader, TableBody, TableHead, TableRow, TableCell,
} from "@/components/ui/table"
import { dashboardApi, type DashboardResponse } from "@/lib/api/dashboard"
import { tokenApi, type TokenConfig } from "@/lib/api/tokens"

function formatNumber(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M"
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "K"
  return n.toString()
}

const statCards = [
  { key: "today_tokens", label: "今日 Token", icon: Activity, color: "text-blue-500", bg: "bg-blue-500/10" },
  { key: "today_requests", label: "今日请求", icon: Send, color: "text-emerald-500", bg: "bg-emerald-500/10" },
  { key: "today_errors", label: "今日错误", icon: AlertCircle, color: "text-red-500", bg: "bg-red-500/10" },
  { key: "available_keys", label: "可用 Key", icon: KeyRound, color: "text-amber-500", bg: "bg-amber-500/10" },
] as const

const PIE_COLORS = [
  "var(--color-primary)", "#10b981", "#f59e0b", "#ef4444", "#8b5cf6",
  "#06b6d4", "#ec4899", "#84cc16",
]

export default function Dashboard() {
  const [data, setData] = useState<DashboardResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")
  const [tokenConsumption, setTokenConsumption] = useState<{
    name: string; value: number
  }[]>([])

  const fetchData = useCallback(async () => {
    setLoading(true)
    setError("")
    try {
      const res = await dashboardApi.get()
      setData(res)
    } catch (err: unknown) {
      const msg =
        (err as { response?: { data?: { message?: string } } })?.response?.data
          ?.message || "加载仪表盘数据失败"
      setError(msg)
    } finally {
      setLoading(false)
    }
  }, [])

  const fetchTokenConsumption = useCallback(async () => {
    try {
      const tokens = await tokenApi.list()
      const consumption = tokens
        .filter((t: TokenConfig) => t.used_quota > 0)
        .sort((a: TokenConfig, b: TokenConfig) => b.used_quota - a.used_quota)
        .slice(0, 8)
        .map((t: TokenConfig) => ({
          name: t.name || `Token #${t.id}`,
          value: t.used_quota,
        }))
      setTokenConsumption(consumption)
    } catch {
      // optional — ignore
    }
  }, [])

  useEffect(() => {
    fetchData()
    fetchTokenConsumption()
  }, [fetchData, fetchTokenConsumption])

  if (loading && !data) {
    return (
      <div className="flex h-full items-center justify-center py-20">
        <div className="flex flex-col items-center gap-3">
          <div className="h-8 w-8 animate-spin rounded-full border-2 border-primary border-t-transparent" />
          <p className="text-sm text-muted-foreground">加载中...</p>
        </div>
      </div>
    )
  }

  if (error && !data) {
    return (
      <div className="space-y-6">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">仪表盘</h1>
          <p className="text-sm text-muted-foreground mt-1">系统概览和关键指标</p>
        </div>
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-16">
            <AlertCircle className="h-12 w-12 text-destructive mb-4" />
            <p className="text-lg font-medium text-destructive">{error}</p>
            <Button variant="outline" className="mt-4" onClick={fetchData}>
              <RefreshCw className="mr-2 h-4 w-4" /> 重试
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  const overview = data?.overview
  const topModels = data?.top_models ?? []
  const recentRequests = data?.recent_requests ?? []

  const chartData = topModels.slice(0, 8).map(m => ({
    name: m.model.length > 12 ? m.model.slice(0, 10) + "…" : m.model,
    fullName: m.model,
    tokens: m.total_tokens,
    requests: m.request_count,
  }))

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">仪表盘</h1>
          <p className="text-sm text-muted-foreground mt-1">系统概览和关键指标</p>
        </div>
        <Button variant="outline" size="sm" onClick={fetchData} disabled={loading}>
          <RefreshCw className={`mr-2 h-4 w-4 ${loading ? "animate-spin" : ""}`} />
          刷新
        </Button>
      </div>

      {/* Stat Cards */}
      <div className="grid gap-4 grid-cols-1 sm:grid-cols-2 lg:grid-cols-4">
        {statCards.map(({ key, label, icon: Icon, color, bg }) => (
          <Card key={key}>
            <CardContent className="p-6">
              <div className="flex items-center justify-between">
                <div>
                  <p className="text-sm font-medium text-muted-foreground">{label}</p>
                  <p className="text-3xl font-bold mt-2">
                    {formatNumber(overview?.[key] ?? 0)}
                  </p>
                </div>
                <div className={`flex h-12 w-12 items-center justify-center rounded-xl ${bg}`}>
                  <Icon className={`h-6 w-6 ${color}`} />
                </div>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {/* Middle Section: Chart + Top Models */}
      <div className="grid gap-4 lg:grid-cols-5">
        {/* Token Usage Chart */}
        <Card className="lg:col-span-3">
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <TrendingUp className="h-4 w-4 text-muted-foreground" />
              模型 Token 用量
            </CardTitle>
            <CardDescription>Top 模型 Token 消耗排行</CardDescription>
          </CardHeader>
          <CardContent>
            {chartData.length === 0 ? (
              <div className="flex h-[300px] items-center justify-center text-sm text-muted-foreground">
                暂无用量数据
              </div>
            ) : (
              <ResponsiveContainer width="100%" height={300}>
                <BarChart data={chartData} margin={{ top: 5, right: 5, bottom: 5, left: 0 }}>
                  <CartesianGrid strokeDasharray="3 3" stroke="var(--color-border)" vertical={false} />
                  <XAxis
                    dataKey="name"
                    tick={{ fill: "var(--color-muted-foreground)", fontSize: 12 }}
                    tickLine={false}
                    axisLine={{ stroke: "var(--color-border)" }}
                  />
                  <YAxis
                    tick={{ fill: "var(--color-muted-foreground)", fontSize: 12 }}
                    tickLine={false}
                    axisLine={false}
                    tickFormatter={(v) => formatNumber(v)}
                  />
                  <Tooltip
                    cursor={{ fill: "var(--color-muted)", opacity: 0.3 }}
                    contentStyle={{
                      backgroundColor: "var(--color-popover)",
                      border: "1px solid var(--color-border)",
                      borderRadius: "0.5rem",
                      color: "var(--color-popover-foreground)",
                      fontSize: "13px",
                    }}
                    formatter={(value: number) => [formatNumber(value), "Token"]}
                    labelFormatter={(_label: unknown, payload: Array<{ payload?: { fullName?: string } }>) => {
                      const p = payload?.[0]?.payload
                      return p?.fullName ?? ""
                    }}
                  />
                  <Bar
                    dataKey="tokens"
                    fill="var(--color-primary)"
                    radius={[4, 4, 0, 0]}
                    maxBarSize={48}
                  />
                </BarChart>
              </ResponsiveContainer>
            )}
          </CardContent>
        </Card>

        {/* Top Models Table */}
        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle className="text-base">Top 模型排行</CardTitle>
            <CardDescription>按 Token 消耗排序</CardDescription>
          </CardHeader>
          <CardContent>
            {topModels.length === 0 ? (
              <div className="flex h-[300px] items-center justify-center text-sm text-muted-foreground">
                暂无数据
              </div>
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>模型</TableHead>
                    <TableHead className="text-right">Token</TableHead>
                    <TableHead className="text-right">请求</TableHead>
                    <TableHead className="text-right">消耗</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {topModels.slice(0, 6).map((m, i) => (
                    <TableRow key={i}>
                      <TableCell className="font-medium max-w-[120px] truncate" title={m.model}>
                        {m.model}
                      </TableCell>
                      <TableCell className="text-right tabular-nums">{formatNumber(m.total_tokens)}</TableCell>
                      <TableCell className="text-right tabular-nums">{formatNumber(m.request_count)}</TableCell>
                      <TableCell className="text-right tabular-nums text-muted-foreground">{formatNumber(m.total_cost)}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Token Consumption Distribution */}
      {tokenConsumption.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <PieChartIcon className="h-4 w-4 text-muted-foreground" />
              令牌消耗分布
            </CardTitle>
            <CardDescription>各令牌已用额度占比</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="flex flex-col lg:flex-row items-center gap-6">
              <ResponsiveContainer width="100%" height={260}>
                <PieChart>
                  <Pie
                    data={tokenConsumption}
                    cx="50%"
                    cy="50%"
                    innerRadius={60}
                    outerRadius={100}
                    paddingAngle={2}
                    dataKey="value"
                    nameKey="name"
                    label={({ name, percent }) =>
                      `${name.length > 8 ? name.slice(0, 6) + "…" : name} ${(percent * 100).toFixed(0)}%`
                    }
                    labelLine={false}
                  >
                    {tokenConsumption.map((_, i) => (
                      <Cell key={i} fill={PIE_COLORS[i % PIE_COLORS.length]} />
                    ))}
                  </Pie>
                  <Tooltip
                    contentStyle={{
                      backgroundColor: "var(--color-popover)",
                      border: "1px solid var(--color-border)",
                      borderRadius: "0.5rem",
                      color: "var(--color-popover-foreground)",
                      fontSize: "13px",
                    }}
                    formatter={(value: number) => [formatNumber(value), "已用额度"]}
                  />
                </PieChart>
              </ResponsiveContainer>
              <div className="flex flex-col gap-2 min-w-[180px]">
                {tokenConsumption.map((item, i) => (
                  <div key={item.name} className="flex items-center gap-2 text-sm">
                    <div
                      className="h-3 w-3 rounded-sm shrink-0"
                      style={{ backgroundColor: PIE_COLORS[i % PIE_COLORS.length] }}
                    />
                    <span className="truncate max-w-[120px]" title={item.name}>{item.name}</span>
                    <span className="ml-auto tabular-nums text-muted-foreground">{formatNumber(item.value)}</span>
                  </div>
                ))}
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Recent Requests */}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">最近请求</CardTitle>
          <CardDescription>最新 10 条请求记录</CardDescription>
        </CardHeader>
        <CardContent>
          {recentRequests.length === 0 ? (
            <div className="flex h-32 items-center justify-center text-sm text-muted-foreground">
              暂无请求记录
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>时间</TableHead>
                  <TableHead>模型</TableHead>
                  <TableHead>渠道</TableHead>
                  <TableHead>状态</TableHead>
                  <TableHead className="text-right">Token</TableHead>
                  <TableHead className="text-right">耗时</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {recentRequests.map((r) => (
                  <TableRow key={r.id}>
                    <TableCell className="text-muted-foreground whitespace-nowrap">
                      {r.created_at}
                    </TableCell>
                    <TableCell className="font-medium max-w-[140px] truncate" title={r.normalized_model ?? r.client_model ?? ""}>
                      {r.normalized_model ?? r.client_model ?? "—"}
                    </TableCell>
                    <TableCell className="max-w-[120px] truncate" title={r.channel_name ?? ""}>
                      {r.channel_name ?? "—"}
                    </TableCell>
                    <TableCell>
                      {r.response_status >= 400 ? (
                        <Badge variant="destructive">{r.response_status}</Badge>
                      ) : (
                        <Badge variant="success">{r.response_status}</Badge>
                      )}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">{formatNumber(r.quota_cost)}</TableCell>
                    <TableCell className="text-right tabular-nums text-muted-foreground">
                      {r.duration_ms}ms
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

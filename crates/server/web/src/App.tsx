import { useEffect, useState } from "react"
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom"
import { useAuthStore } from "@/stores/auth"
import { authApi } from "@/lib/api"
import { setupApi } from "@/lib/api/setup"
import { Toaster } from "@/components/ui/toaster"
import Layout from "@/components/layout/Layout"
import Login from "@/pages/Login"
import Setup from "@/pages/Setup"
import Dashboard from "@/pages/Dashboard"
import Channels from "@/pages/Channels"
import Models from "@/pages/Models"
import Users from "@/pages/Users"
import Tokens from "@/pages/Tokens"
import Usage from "@/pages/Usage"
import Logs from "@/pages/Logs"
import Pricing from "@/pages/Pricing"

function FullScreenSpinner() {
  return (
    <div className="flex h-screen items-center justify-center">
      <div className="flex flex-col items-center gap-3">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-primary border-t-transparent" />
        <p className="text-sm text-muted-foreground">加载中...</p>
      </div>
    </div>
  )
}

function ProtectedLayout() {
  const { user } = useAuthStore()
  if (!user) {
    return <Navigate to="/admin/login" replace />
  }
  return <Layout />
}

function App() {
  const setUser = useAuthStore((s) => s.setUser)
  const setLoading = useAuthStore((s) => s.setLoading)
  const loading = useAuthStore((s) => s.loading)
  // null = 未确定，true = 需初始化，false = 已初始化
  const [needsSetup, setNeedsSetup] = useState<boolean | null>(null)

  useEffect(() => {
    // 顺序调用：先查 setup 状态，仅当不需初始化时才调 me()，
    // 避免 needs_setup=true 时 me() 返回 401 触发拦截器硬跳转。
    setupApi
      .getStatus()
      .then((res) => {
        if (res.needs_setup) {
          setNeedsSetup(true)
          setLoading(false)
        } else {
          setNeedsSetup(false)
          authApi
            .me()
            .then((r) => setUser(r.data.user))
            .catch(() => setUser(null))
            .finally(() => setLoading(false))
        }
      })
      .catch(() => {
        // getStatus 网络错误：降级进入正常登录流程，不阻塞用户
        setNeedsSetup(false)
        authApi
          .me()
          .then((r) => setUser(r.data.user))
          .catch(() => setUser(null))
          .finally(() => setLoading(false))
      })
  }, [setUser, setLoading])

  // 等待 setup 状态和 loading 确定后再渲染路由
  if (loading || needsSetup === null) {
    return <FullScreenSpinner />
  }

  return (
    <BrowserRouter>
      <Routes>
        <Route
          path="/setup"
          element={needsSetup ? <Setup /> : <Navigate to="/admin/login" replace />}
        />
        <Route
          path="/admin/login"
          element={needsSetup ? <Navigate to="/setup" replace /> : <Login />}
        />
        <Route
          path="/admin"
          element={needsSetup ? <Navigate to="/setup" replace /> : <ProtectedLayout />}
        >
          <Route index element={<Navigate to="/admin/dashboard" replace />} />
          <Route path="dashboard" element={<Dashboard />} />
          <Route path="channels" element={<Channels />} />
          <Route path="models" element={<Models />} />
          <Route path="users" element={<Users />} />
          <Route path="tokens" element={<Tokens />} />
          <Route path="usage" element={<Usage />} />
          <Route path="logs" element={<Logs />} />
          <Route path="pricing" element={<Pricing />} />
        </Route>
        <Route path="*" element={<Navigate to="/admin" replace />} />
      </Routes>
      <Toaster />
    </BrowserRouter>
  )
}

export default App

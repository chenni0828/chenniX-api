import { Link, useLocation } from "react-router-dom"
import {
  LayoutDashboard,
  Radio,
  Boxes,
  Users,
  KeyRound,
  BarChart3,
  ScrollText,
  ChevronLeft,
  Zap,
  DollarSign,
} from "lucide-react"
import { cn } from "@/lib/utils"
import { useAuthStore } from "@/stores/auth"

interface NavItem {
  label: string
  to: string
  icon: React.ComponentType<{ className?: string }>
  adminOnly?: boolean
}

const navItems: NavItem[] = [
  { label: "仪表盘", to: "/admin/dashboard", icon: LayoutDashboard },
  { label: "渠道管理", to: "/admin/channels", icon: Radio },
  { label: "模型管理", to: "/admin/models", icon: Boxes },
  { label: "定价管理", to: "/admin/pricing", icon: DollarSign, adminOnly: true },
  { label: "用户管理", to: "/admin/users", icon: Users, adminOnly: true },
  { label: "令牌管理", to: "/admin/tokens", icon: KeyRound },
  { label: "用量统计", to: "/admin/usage", icon: BarChart3 },
  { label: "请求日志", to: "/admin/logs", icon: ScrollText },
]

interface SidebarProps {
  collapsed: boolean
  onToggle: () => void
}

export default function Sidebar({ collapsed, onToggle }: SidebarProps) {
  const location = useLocation()
  const user = useAuthStore((s) => s.user)

  const visibleItems = navItems.filter(
    (item) => !item.adminOnly || (user && user.role >= 10)
  )

  return (
    <aside
      className={cn(
        "flex h-screen flex-col border-r bg-sidebar transition-all duration-300 ease-in-out",
        collapsed ? "w-16" : "w-60"
      )}
    >
      {/* Logo */}
      <div className="flex h-14 items-center gap-2 border-b px-4">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground">
          <Zap className="h-5 w-5" />
        </div>
        {!collapsed && (
          <span className="text-lg font-bold tracking-tight">chennix</span>
        )}
      </div>

      {/* Navigation */}
      <nav className="flex-1 space-y-1 overflow-y-auto p-2">
        {visibleItems.map((item) => {
          const isActive = location.pathname === item.to
          const Icon = item.icon
          return (
            <Link
              key={item.to}
              to={item.to}
              className={cn(
                "flex items-center gap-3 rounded-md px-3 py-2 text-sm font-medium transition-colors",
                isActive
                  ? "bg-sidebar-primary text-sidebar-primary-foreground"
                  : "text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
                collapsed && "justify-center px-2"
              )}
              title={collapsed ? item.label : undefined}
            >
              <Icon className="h-5 w-5 shrink-0" />
              {!collapsed && <span>{item.label}</span>}
            </Link>
          )
        })}
      </nav>

      {/* Collapse toggle */}
      <div className="border-t p-2">
        <button
          onClick={onToggle}
          className={cn(
            "flex w-full items-center rounded-md px-3 py-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground",
            collapsed ? "justify-center" : "justify-end"
          )}
        >
          <ChevronLeft
            className={cn(
              "h-4 w-4 transition-transform",
              collapsed && "rotate-180"
            )}
          />
          {!collapsed && <span className="ml-2">收起</span>}
        </button>
      </div>
    </aside>
  )
}

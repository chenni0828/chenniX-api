import { useState } from "react"
import { useNavigate } from "react-router-dom"
import {
  Menu,
  Moon,
  Sun,
  LogOut,
  User as UserIcon,
  ChevronDown,
  KeyRound,
  Loader2,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  Dialog, DialogContent, DialogDescription, DialogFooter,
  DialogHeader, DialogTitle,
} from "@/components/ui/dialog"
import { useAuthStore } from "@/stores/auth"
import { authApi } from "@/lib/api"
import { userApi } from "@/lib/api/users"
import { toast } from "@/hooks/use-toast"
import { getErrorMessage } from "@/lib/format"

interface TopbarProps {
  onToggleSidebar: () => void
}

function roleLabel(role: number): string {
  if (role >= 100) return "超级管理员"
  if (role >= 10) return "管理员"
  return "普通用户"
}

export default function Topbar({ onToggleSidebar }: TopbarProps) {
  const navigate = useNavigate()
  const user = useAuthStore((s) => s.user)
  const logout = useAuthStore((s) => s.logout)

  const [pwdOpen, setPwdOpen] = useState(false)
  const [oldPwd, setOldPwd] = useState("")
  const [newPwd, setNewPwd] = useState("")
  const [confirmPwd, setConfirmPwd] = useState("")
  const [pwdLoading, setPwdLoading] = useState(false)

  const toggleDark = () => {
    document.documentElement.classList.toggle("dark")
  }

  const handleLogout = async () => {
    try {
      await authApi.logout()
    } catch {
      // ignore network errors
    }
    logout()
    toast({ title: "已退出登录" })
    navigate("/admin/login")
  }

  const handleChangePassword = async () => {
    if (!oldPwd || !newPwd) {
      toast({ title: "请填写旧密码和新密码", variant: "destructive" })
      return
    }
    if (newPwd !== confirmPwd) {
      toast({ title: "两次输入的新密码不一致", variant: "destructive" })
      return
    }
    setPwdLoading(true)
    try {
      await userApi.updateMyPassword(oldPwd, newPwd)
      toast({ title: "密码修改成功" })
      setPwdOpen(false)
      setOldPwd("")
      setNewPwd("")
      setConfirmPwd("")
    } catch (err) {
      toast({ title: "修改失败", description: getErrorMessage(err, "请重试"), variant: "destructive" })
    } finally {
      setPwdLoading(false)
    }
  }

  return (
    <header className="flex h-14 shrink-0 items-center justify-between border-b bg-background px-4">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="icon" onClick={onToggleSidebar}>
          <Menu className="h-5 w-5" />
        </Button>
      </div>

      <div className="flex items-center gap-2">
        {/* Dark mode toggle */}
        <Button variant="ghost" size="icon" onClick={toggleDark}>
          <Sun className="h-5 w-5 dark:hidden" />
          <Moon className="hidden h-5 w-5 dark:block" />
        </Button>

        {/* User menu */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" className="flex items-center gap-2 px-2">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-primary text-primary-foreground">
                <UserIcon className="h-4 w-4" />
              </div>
              <span className="hidden text-sm font-medium sm:inline">
                {user?.username ?? "未知"}
              </span>
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-56">
            <DropdownMenuLabel>
              <div className="flex flex-col">
                <span className="text-sm font-medium">{user?.username}</span>
                <span className="text-xs text-muted-foreground">
                  {user ? roleLabel(user.role) : ""}
                </span>
              </div>
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onClick={() => {
                setOldPwd("")
                setNewPwd("")
                setConfirmPwd("")
                setPwdOpen(true)
              }}
            >
              <KeyRound className="mr-2 h-4 w-4" />
              修改密码
            </DropdownMenuItem>
            <DropdownMenuItem
              className="text-destructive focus:text-destructive"
              onClick={handleLogout}
            >
              <LogOut className="mr-2 h-4 w-4" />
              退出登录
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {/* Change password dialog */}
      <Dialog open={pwdOpen} onOpenChange={(v) => {
        setPwdOpen(v)
        if (!v) {
          setOldPwd("")
          setNewPwd("")
          setConfirmPwd("")
        }
      }}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>修改密码</DialogTitle>
            <DialogDescription>修改当前账户的登录密码</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="old-pwd">旧密码</Label>
              <Input
                id="old-pwd"
                type="password"
                value={oldPwd}
                onChange={(e) => setOldPwd(e.target.value)}
                placeholder="输入当前密码"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="new-pwd">新密码</Label>
              <Input
                id="new-pwd"
                type="password"
                value={newPwd}
                onChange={(e) => setNewPwd(e.target.value)}
                placeholder="输入新密码"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="confirm-pwd">确认新密码</Label>
              <Input
                id="confirm-pwd"
                type="password"
                value={confirmPwd}
                onChange={(e) => setConfirmPwd(e.target.value)}
                placeholder="再次输入新密码"
                onKeyDown={(e) => { if (e.key === "Enter") handleChangePassword() }}
              />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setPwdOpen(false)} disabled={pwdLoading}>取消</Button>
            <Button onClick={handleChangePassword} disabled={pwdLoading}>
              {pwdLoading && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
              确认修改
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </header>
  )
}

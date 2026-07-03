import {
  Toast,
  ToastClose,
  ToastDescription,
  ToastProvider,
  ToastTitle,
  ToastViewport,
} from "@/components/ui/toast"
import { useToast } from "@/hooks/use-toast"

export function Toaster() {
  const { toasts } = useToast()

  return (
    <ToastProvider>
      {toasts.map(function ({ id, title, description, action, variant, ...props }) {
        // destructive（错误）提示显示更久，方便用户复制错误信息
        const duration = variant === "destructive" ? 12000 : 5000
        return (
          <Toast key={id} variant={variant} duration={duration} {...props}>
            <div className="grid gap-1 min-w-0 flex-1">
              {title && <ToastTitle className="select-text">{title}</ToastTitle>}
              {description && (
                <ToastDescription>{description}</ToastDescription>
              )}
            </div>
            {action}
            <ToastClose />
          </Toast>
        )
      })}
      <ToastViewport />
    </ToastProvider>
  )
}

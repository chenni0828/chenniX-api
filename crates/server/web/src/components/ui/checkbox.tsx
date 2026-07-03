import * as React from "react"
import { Check } from "lucide-react"
import { cn } from "@/lib/utils"

const Checkbox = React.forwardRef<
  HTMLButtonElement,
  React.ComponentPropsWithoutRef<"button"> & { checked?: boolean }
>(({ className, checked, onClick, ...props }, ref) => (
  <button
    ref={ref}
    type="button"
    role="checkbox"
    aria-checked={checked}
    data-state={checked ? "checked" : "unchecked"}
    className={cn(
      "peer h-4 w-4 shrink-0 rounded-[4px] border border-primary ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50",
      checked && "bg-primary text-primary-foreground",
      className
    )}
    onClick={onClick}
    {...props}
  >
    {checked && <Check className="h-3 w-3" strokeWidth={3} />}
  </button>
))
Checkbox.displayName = "Checkbox"

export { Checkbox }

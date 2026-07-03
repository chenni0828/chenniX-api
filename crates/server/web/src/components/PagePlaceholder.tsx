import { Card, CardContent } from "@/components/ui/card"
import { Construction } from "lucide-react"

interface PagePlaceholderProps {
  title: string
  description?: string
}

export default function PagePlaceholder({ title, description }: PagePlaceholderProps) {
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">{title}</h1>
        {description && (
          <p className="text-sm text-muted-foreground mt-1">{description}</p>
        )}
      </div>
      <Card>
        <CardContent className="flex flex-col items-center justify-center py-16">
          <Construction className="h-12 w-12 text-muted-foreground mb-4" />
          <p className="text-lg font-medium text-muted-foreground">开发中</p>
          <p className="text-sm text-muted-foreground/70 mt-1">
            此功能正在开发中，敬请期待
          </p>
        </CardContent>
      </Card>
    </div>
  )
}

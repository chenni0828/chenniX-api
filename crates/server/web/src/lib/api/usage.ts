import api from "@/lib/api"

export interface UsageSummary {
  channel_id: number
  channel_name: string
  model: string
  total_tokens: number
  request_count: number
  total_cost: number
}

export interface UsageQuery {
  channel_id?: number
  model?: string
  start?: number
  end?: number
}

export const usageApi = {
  get: (params: UsageQuery) =>
    api.get<UsageSummary[]>("/usage", { params }).then((r) => r.data),
}

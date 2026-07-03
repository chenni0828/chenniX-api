import api from "@/lib/api"

export interface RequestLog {
  id: number
  request_id: string
  client_ip: string | null
  method: string
  path: string
  client_model: string | null
  normalized_model: string | null
  channel_name: string | null
  key_label: string | null
  upstream_status: number | null
  response_status: number
  duration_ms: number
  stream: boolean
  user_id: number | null
  token_id: number | null
  quota_cost: number
  error_message: string | null
  created_at: string
}

export interface LogsResponse {
  logs: RequestLog[]
  total: number
  page: number
  per_page: number
}

export interface LogsQuery {
  page?: number
  per_page?: number
  channel_id?: number
  model?: string
  status_code?: number
  start?: number
  end?: number
}

export const logApi = {
  get: (params: LogsQuery) =>
    api.get<LogsResponse>("/logs", { params }).then((r) => r.data),
}

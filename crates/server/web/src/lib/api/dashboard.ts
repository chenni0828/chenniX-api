import api from '../api'

export interface DashboardOverview {
  today_tokens: number
  today_requests: number
  today_errors: number
  available_keys: number
}

export interface ModelUsage {
  model: string
  total_tokens: number
  request_count: number
  total_cost: number
}

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

export interface DashboardResponse {
  overview: DashboardOverview
  top_models: ModelUsage[]
  recent_requests: RequestLog[]
}

export const dashboardApi = {
  get: () => api.get<DashboardResponse>('/dashboard').then(r => r.data),
}

import api from "@/lib/api"

export interface TokenConfig {
  id: number
  user_id: number
  key: string
  name: string | null
  remain_quota: number
  used_quota: number
  unlimited_quota: boolean
  expired_time: number
  model_limits_enabled: boolean
  model_limits: string[] | null
  status: number
  allow_ips: string[] | null
}

export interface CreateTokenPayload {
  user_id?: number
  name: string
  key: string
  remain_quota: number
  unlimited_quota: boolean
  expired_time: number
  model_limits: string
  model_limits_enabled: boolean
  allow_ips: string
}

export interface TokenUsage {
  total_tokens: number
  request_count: number
  last_used_at: number
}

export interface UpdateTokenPayload {
  name: string
  remain_quota: number
  unlimited_quota: boolean
  expired_time: number
  model_limits: string
  model_limits_enabled: boolean
  allow_ips: string
  status: number
}

export const tokenApi = {
  list: (userId?: number) =>
    api.get<TokenConfig[]>("/tokens", { params: { user_id: userId } }).then((r) => r.data),
  create: (data: CreateTokenPayload, userId?: number) =>
    api.post("/tokens", data, { params: userId ? { user_id: userId } : {} }).then((r) => r.data),
  update: (id: number, data: UpdateTokenPayload) =>
    api.put(`/tokens/${id}`, data).then((r) => r.data),
  delete: (id: number) => api.delete(`/tokens/${id}`).then((r) => r.data),
  getUsage: (id: number) =>
    api.get<TokenUsage>(`/tokens/${id}/usage`).then((r) => r.data),
}

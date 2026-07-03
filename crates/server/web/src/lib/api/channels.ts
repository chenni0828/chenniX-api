import api from "@/lib/api"
import type { ChannelModelPricing } from "@/lib/api/pricing"

export interface ChannelConfig {
  id: number
  name: string
  provider: string
  base_url: string
  group: string
}

export interface KeyConfig {
  id: number
  channel_id: number
  api_key: string
  label: string | null
  cost_tier: string
  key_priority: number
  price_per_1k_tokens: number | null
  free_quota: number | null
  used_quota: number
  quota_reset_period: string | null
  status: string
}

export interface CreateChannelData {
  name: string
  provider: string
  base_url: string
  group: string
}

export interface CreateKeyData {
  api_key: string
  is_free: boolean
  priority: number
  quota_limit: number
  price_per_1k_tokens: number
}

export interface UpdateKeyData extends CreateKeyData {
  status: string
}

export interface ChannelModel {
  model_id: number | null
  canonical_name: string
  upstream_model_name?: string | null
  priority: number
  pricing?: ChannelModelPricing
}
export interface ChannelTestResult {
  success: boolean
  latency_ms: number
  error: string | null
}

export interface DiscoverModelsResult {
  models: string[]
}

export const channelApi = {
  list: () => api.get<ChannelConfig[]>("/channels").then((r) => r.data),
  create: (data: CreateChannelData) => api.post<number>("/channels", data).then((r) => r.data),
  update: (id: number, data: CreateChannelData) => api.put(`/channels/${id}`, data).then((r) => r.data),
  delete: (id: number) => api.delete(`/channels/${id}`).then((r) => r.data),
  listKeys: (channelId: number) => api.get<KeyConfig[]>(`/channels/${channelId}/keys`).then((r) => r.data),
  createKey: (channelId: number, data: CreateKeyData) => api.post<number>(`/channels/${channelId}/keys`, data).then((r) => r.data),
  updateKey: (channelId: number, keyId: number, data: UpdateKeyData) => api.put(`/channels/${channelId}/keys/${keyId}`, data).then((r) => r.data),
  deleteKey: (channelId: number, keyId: number) => api.delete(`/channels/${channelId}/keys/${keyId}`).then((r) => r.data),
  reload: () => api.post("/reload").then((r) => r.data),
  testChannel: (channelId: number) => api.post<ChannelTestResult>(`/channels/${channelId}/test`).then((r) => r.data),
  getModels: (channelId: number) => api.get<ChannelModel[]>(`/channels/${channelId}/models`).then((r) => r.data),
  discoverModels: (baseUrl: string, apiKey: string, provider: string) =>
    api.post<DiscoverModelsResult>("/discover-models", {
      base_url: baseUrl,
      api_key: apiKey,
      provider,
    }).then((r) => r.data),
  discoverModelsByChannel: (channelId: number) =>
    api.post<DiscoverModelsResult>(`/channels/${channelId}/discover-models`).then((r) => r.data),
  /** 批量将选中的 discovered 模型加入小模型池 (discovered_models 表) */
  addDiscoveredModels: (channelId: number, models: string[]) =>
    api.post<{ added: number }>(`/channels/${channelId}/discovered-models`, { models }).then((r) => r.data),
  /** 从小模型池移除指定发现模型 (binding_count>0 时后端会拒绝) */
  deleteDiscoveredModel: (channelId: number, upstreamModelName: string) =>
    api.delete(`/channels/${channelId}/discovered-models/${encodeURIComponent(upstreamModelName)}`).then((r) => r.data),
  addChannelModel: (channelId: number, modelName: string, upstreamModelName?: string) =>
    api.post(`/channels/${channelId}/models`, {
      model_name: modelName,
      upstream_model_name: upstreamModelName || modelName,
    }).then((r) => r.data),
  removeChannelModel: (channelId: number, modelName: string) =>
    api.delete(`/channels/${channelId}/models/${encodeURIComponent(modelName)}`).then((r) => r.data),
}

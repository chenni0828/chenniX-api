import api from '../api'
import type { ChannelTestResult } from './channels'

// ===== Routing & Quota types =====

export type RoutingStrategy = 'priority' | 'load_balance'

export type QuotaUnit = 'token' | 'call'

export type QuotaWindow = 'day' | 'month' | 'total'

export type QuotaStatus = 'available' | 'exhausted'

export interface QuotaState {
  quota_limit: number | null
  quota_unit: QuotaUnit | null
  quota_window: QuotaWindow | null
  used_quota: number
  last_reset_at: string | null
  quota_status: QuotaStatus
}

export interface SmallModel {
  channel_id: number
  raw_model_name: string
  quota_limit: number | null
  quota_unit: QuotaUnit | null
  quota_window: QuotaWindow | null
  used_quota: number
  last_reset_at: string | null
  quota_status: QuotaStatus
  binding_count: number
}

export interface UpdateSmallModelQuotaData {
  limit: number
  unit: QuotaUnit
  window: QuotaWindow
}

// ===== Model & Binding types =====

export interface ModelBindingInfo {
  channel_id: number
  upstream_model_name: string
  channel_name?: string | null
  priority: number
  weight: number
}

export interface ModelInfo {
  id: number
  canonical_name: string
  routing_strategy: RoutingStrategy
  bindings: ModelBindingInfo[]
}

export interface TestModelResult {
  success: boolean
  latency_ms: number
  error: string | null
}

export interface CreateModelData {
  canonical_name: string
}

export interface UpdateModelData {
  canonical_name?: string
}

export const modelApi = {
  list: () => api.get<ModelInfo[]>('/models').then(r => r.data),
  create: (data: CreateModelData) => api.post<number>('/models', data).then(r => r.data),
  /** 仅创建 models 行；返回新建的完整模型对象 */
  createModel: (name: string) =>
    api.post<ModelInfo>('/models', { canonical_name: name }).then(r => r.data),
  update: (id: number, data: UpdateModelData) => api.put(`/models/${id}`, data).then(r => r.data),
  delete: (id: number) => api.delete(`/models/${id}`).then(r => r.data),
  /** 切换大模型的路由策略（priority / load_balance） */
  updateRoutingStrategy: (modelId: number, strategy: RoutingStrategy): Promise<void> =>
    api.patch(`/models/${modelId}/strategy`, { strategy }).then(r => r.data),
  testModel: (modelId: number) =>
    api.post<TestModelResult>(`/models/${modelId}/test`).then(r => r.data),
  addBinding: (modelId: number, channelId: number, upstreamModelName: string) =>
    api.post(`/models/${modelId}/bindings`, { channel_id: channelId, upstream_model_name: upstreamModelName }).then(r => r.data),
  /** 调整某条绑定的权重（load_balance 策略下使用） */
  updateBindingWeight: (
    modelId: number,
    channelId: number,
    upstream: string,
    weight: number,
  ): Promise<void> =>
    api
      .patch(`/models/${modelId}/bindings/weight`, {
        channel_id: channelId,
        upstream_model_name: upstream,
        weight,
      })
      .then(r => r.data),
  removeBinding: (modelId: number, channelId: number) =>
    api.delete(`/models/${modelId}/bindings/${channelId}`).then(r => r.data),
  reorder: (
    modelId: number,
    bindings: { channel_id: number; upstream_model_name: string }[],
  ) =>
    api.put(`/models/${modelId}/bindings/reorder`, { bindings }).then(r => r.data),
  testBinding: (modelId: number, channelId: number, upstream: string) =>
    api
      .post<ChannelTestResult>(
        `/models/${modelId}/bindings/${channelId}/${encodeURIComponent(upstream)}/test`,
      )
      .then(r => r.data),
  /** 列出所有渠道下发现的小模型（discovered_models）及其额度状态与已绑定大模型计数 */
  listSmallModels: () => api.get<SmallModel[]>('/small-models').then(r => r.data),
  /** 设置某渠道下指定上游小模型的额度配置 */
  updateSmallModelQuota: (
    channelId: number,
    upstream: string,
    quota: UpdateSmallModelQuotaData,
  ): Promise<void> =>
    api
      .patch(`/channels/${channelId}/models/${encodeURIComponent(upstream)}/quota`, quota)
      .then(r => r.data),
  /** 重置某渠道下指定上游小模型的额度用量 */
  resetSmallModelQuota: (channelId: number, upstream: string): Promise<void> =>
    api
      .post(`/channels/${channelId}/models/${encodeURIComponent(upstream)}/quota/reset`)
      .then(r => r.data),
}

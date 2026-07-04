import api from '@/lib/api'

/**
 * 计费模式：
 * - Token: 按 input/output tokens 计费（元/1K tokens）
 * - PerCall: 按调用次数计费（元/次）
 * - Expression: 按表达式计费（结果单位为元）
 *
 * 表达式可用变量：p (prompt tokens), c (completion tokens), total (p + c)
 * 表达式语法：evalexpr v13（支持 + - * / % 比较运算、if(cond, then, else) 函数等）
 * 示例：
 *   - 按 token：`p / 1000 * 0.001 + c / 1000 * 0.002`
 *   - 分段：`if(total > 10000, p * 0.0000005 + c * 0.000001, p * 0.000001 + c * 0.000002)`
 *   - 按次：`1.5`
 */
export type BillingType = 'Token' | 'PerCall' | 'Expression'

export const BillingTypeToken = 'Token' as const
export const BillingTypePerCall = 'PerCall' as const
export const BillingTypeExpression = 'Expression' as const

export interface ChannelModelPricing {
  billing_type: BillingType
  /** Token 模式：元/1K tokens */
  input_price: number
  /** Token 模式：元/1K tokens */
  output_price: number
  /** PerCall 模式：元/次 */
  call_price: number
  /** Expression 模式：表达式字符串 */
  billing_expr: string | null
}

export function emptyPricing(): ChannelModelPricing {
  return {
    billing_type: BillingTypeToken,
    input_price: 0,
    output_price: 0,
    call_price: 0,
    billing_expr: null,
  }
}

export interface BindingPricingRow {
  model_id: number
  canonical_name: string
  channel_id: number
  channel_name: string
  upstream_model_name: string | null
  priority: number
  pricing: ChannelModelPricing
}

export interface UpdateBindingPricingData {
  channel_id: number
  upstream_model_name: string
  pricing: ChannelModelPricing
}

export const pricingApi = {
  /** 列出所有渠道-模型绑定及其定价 */
  list: () => api.get<BindingPricingRow[]>('/pricing').then((r) => r.data),
  /** 更新某个 (model_id, channel_id, upstream_model_name) 三元组绑定的定价 */
  update: (modelId: number, data: UpdateBindingPricingData) =>
    api.put(`/models/${modelId}/pricing`, data).then((r) => r.data),
}

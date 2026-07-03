//! 计费表达式引擎。
//!
//! 使用 [`evalexpr`] 对用户配置的表达式进行求值，支持按 token / 分段 / 按次
//! 等灵活计费规则。表达式返回值单位为「元」，直接作为请求费用。
//!
//! ## 可用变量
//!
//! | 变量 | 含义 |
//! |------|------|
//! | `p` | prompt tokens (输入) |
//! | `c` | completion tokens (输出) |
//! | `total` | 总 tokens (p + c) |
//!
//! ## 表达式示例
//!
//! - 按 token（系数为 元/1K tokens，需自行除以 1000）：
//!   `p / 1000 * 0.001 + c / 1000 * 0.002`
//! - 分段（超过 1 万 token 时半价）：
//!   `if(total > 10000, p * 0.0000005 + c * 0.000001, p * 0.000001 + c * 0.000002)`
//! - 按次固定费用：
//!   `1.5`
//!
//! ## 单位约定
//!
//! 表达式求值结果是「元」。`p`/`c`/`total` 是 token 计数（整数转 f64）。
//! 因此按 token 计价时系数应是 元/token（即 元/1K tokens ÷ 1000）。

use chennix_common::{ProxyError, ProxyResult};
use evalexpr::{context_map, DefaultNumericTypes, HashMapContext, Value};

/// 评估计费表达式，返回费用（单位：元）。
///
/// `prompt_tokens` 和 `completion_tokens` 会以 f64 注入表达式上下文，
/// 表达式必须返回数值（Int 或 Float），否则返回错误。
pub fn eval(expr: &str, prompt_tokens: u64, completion_tokens: u64) -> ProxyResult<f64> {
    let ctx: HashMapContext<DefaultNumericTypes> = context_map! {
        "p" => evalexpr::Value::from_float(prompt_tokens as f64),
        "c" => evalexpr::Value::from_float(completion_tokens as f64),
        "total" => evalexpr::Value::from_float((prompt_tokens + completion_tokens) as f64),
    }
    .map_err(|e| ProxyError::Config(format!("billing expr ctx: {}", e)))?;

    let val = evalexpr::eval_with_context(expr, &ctx)
        .map_err(|e| ProxyError::Config(format!("billing expr eval failed: {}", e)))?;
    match val {
        Value::Float(f) => Ok(f),
        Value::Int(i) => Ok(i as f64),
        other => Err(ProxyError::Config(format!(
            "billing expr must return number, got {:?}",
            other
        ))),
    }
}

/// 验证表达式语法与语义（保存前调用）。
///
/// 使用一组虚拟 token 数对表达式进行试求值，任何错误都返回 `Err`。
pub fn validate(expr: &str) -> ProxyResult<()> {
    if expr.trim().is_empty() {
        return Err(ProxyError::Config("billing expr is empty".into()));
    }
    // 多组试算，覆盖常见 token 分布。
    eval(expr, 100, 50)?;
    eval(expr, 0, 0)?;
    eval(expr, 10000, 5000)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_basic_token_pricing() {
        // 0.001 元/1K input + 0.002 元/1K output
        let expr = "p / 1000 * 0.001 + c / 1000 * 0.002";
        let cost = eval(expr, 1000, 500).unwrap();
        // (1000/1000*0.001) + (500/1000*0.002) = 0.001 + 0.001 = 0.002
        assert!((cost - 0.002).abs() < 1e-9, "got {}", cost);
    }

    #[test]
    fn test_eval_fixed_per_call() {
        let expr = "1.5";
        let cost = eval(expr, 12345, 6789).unwrap();
        assert!((cost - 1.5).abs() < 1e-9, "got {}", cost);
    }

    #[test]
    fn test_eval_tiered() {
        // total > 10000 时输入半价：使用 if(condition, then, else) 函数
        let expr =
            "if(total > 10000, p * 0.0000005 + c * 0.000001, p * 0.000001 + c * 0.000002)";
        // 低段：total = 5000 + 3000 = 8000 (<= 10000)
        let low_tier = eval(expr, 5000, 3000).unwrap();
        // 高段：total = 8000 + 5000 = 13000 (> 10000)
        let high_tier = eval(expr, 8000, 5000).unwrap();
        assert!(low_tier > 0.0 && high_tier > 0.0);
        // 高段单 token 价格应低于低段
        let per_tok_low = low_tier / 8000.0;
        let per_tok_high = high_tier / 13000.0;
        assert!(
            per_tok_high < per_tok_low,
            "high tier should be cheaper per token: low={} high={}",
            per_tok_low,
            per_tok_high
        );
    }

    #[test]
    fn test_validate_rejects_empty() {
        assert!(validate("").is_err());
        assert!(validate("   ").is_err());
    }

    #[test]
    fn test_validate_rejects_syntax_error() {
        assert!(validate("p +").is_err());
        assert!(validate("p * c /").is_err());
    }

    #[test]
    fn test_validate_accepts_valid() {
        assert!(validate("p * 0.001").is_ok());
        assert!(validate("if(total > 1000, 1, 2)").is_ok());
    }

    #[test]
    fn test_eval_zero_tokens() {
        let cost = eval("p * 0.001 + c * 0.002", 0, 0).unwrap();
        assert!((cost - 0.0).abs() < 1e-9);
    }
}

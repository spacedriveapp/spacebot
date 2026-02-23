//! Best-effort LLM pricing estimates.
//!
//! Maps model names to per-token costs (USD). These are approximate —
//! actual costs depend on provider agreements, caching, and batching.
//! Unknown models fall back to a conservative default.

/// Per-token pricing for a model.
struct ModelPricing {
    /// Cost per input token in USD.
    input: f64,
    /// Cost per output token in USD.
    output: f64,
    /// Cost per cached input token in USD (typically discounted).
    cached_input: f64,
}

/// Look up pricing for a model name. Matches on the model portion
/// (after the provider/ prefix) so "anthropic/claude-sonnet-4-20250514"
/// and "claude-sonnet-4-20250514" both match.
fn lookup_pricing(model_name: &str) -> ModelPricing {
    let model = model_name
        .split_once('/')
        .map(|(_, m)| m)
        .unwrap_or(model_name);

    let per_m = |price: f64| price / 1_000_000.0;

    match model {
        m if m.starts_with("claude-opus-4") => ModelPricing {
            input: per_m(15.0),
            output: per_m(75.0),
            cached_input: per_m(1.5),
        },
        m if m.starts_with("claude-sonnet-4") => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
        },
        m if m.starts_with("claude-3-5-sonnet") => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
        },
        m if m.starts_with("claude-3-5-haiku") || m.starts_with("claude-haiku-4") => ModelPricing {
            input: per_m(0.80),
            output: per_m(4.0),
            cached_input: per_m(0.08),
        },

        m if m.starts_with("claude-3-opus") => ModelPricing {
            input: per_m(15.0),
            output: per_m(75.0),
            cached_input: per_m(1.5),
        },
        m if m.starts_with("claude-3-sonnet") => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
        },
        m if m.starts_with("claude-3-haiku") => ModelPricing {
            input: per_m(0.25),
            output: per_m(1.25),
            cached_input: per_m(0.03),
        },

        m if m.starts_with("gpt-4o-mini") => ModelPricing {
            input: per_m(0.15),
            output: per_m(0.60),
            cached_input: per_m(0.075),
        },
        m if m.starts_with("gpt-4o") => ModelPricing {
            input: per_m(2.50),
            output: per_m(10.0),
            cached_input: per_m(1.25),
        },
        m if m.starts_with("gpt-4-turbo") => ModelPricing {
            input: per_m(10.0),
            output: per_m(30.0),
            cached_input: per_m(5.0),
        },

        m if m.starts_with("o3-mini") => ModelPricing {
            input: per_m(1.10),
            output: per_m(4.40),
            cached_input: per_m(0.55),
        },
        m if m.starts_with("o3") => ModelPricing {
            input: per_m(10.0),
            output: per_m(40.0),
            cached_input: per_m(5.0),
        },
        m if m.starts_with("o1-mini") => ModelPricing {
            input: per_m(3.0),
            output: per_m(12.0),
            cached_input: per_m(1.5),
        },
        m if m.starts_with("o1") => ModelPricing {
            input: per_m(15.0),
            output: per_m(60.0),
            cached_input: per_m(7.5),
        },

        m if m.starts_with("gemini-2.0-flash") || m.starts_with("gemini-2.5-flash") => {
            ModelPricing {
                input: per_m(0.075),
                output: per_m(0.30),
                cached_input: per_m(0.01875),
            }
        }
        m if m.starts_with("gemini-2.5-pro") || m.starts_with("gemini-2.0-pro") => ModelPricing {
            input: per_m(1.25),
            output: per_m(10.0),
            cached_input: per_m(0.3125),
        },
        m if m.starts_with("gemini-1.5-pro") => ModelPricing {
            input: per_m(1.25),
            output: per_m(5.0),
            cached_input: per_m(0.3125),
        },
        m if m.starts_with("gemini-1.5-flash") => ModelPricing {
            input: per_m(0.075),
            output: per_m(0.30),
            cached_input: per_m(0.01875),
        },

        m if m.starts_with("deepseek-chat") || m.starts_with("deepseek-v3") => ModelPricing {
            input: per_m(0.27),
            output: per_m(1.10),
            cached_input: per_m(0.07),
        },
        m if m.starts_with("deepseek-reasoner") || m.starts_with("deepseek-r1") => ModelPricing {
            input: per_m(0.55),
            output: per_m(2.19),
            cached_input: per_m(0.14),
        },

        _ => ModelPricing {
            input: per_m(3.0),
            output: per_m(15.0),
            cached_input: per_m(0.30),
        },
    }
}

/// Estimate cost in USD for a completion call.
///
/// `cached_input_tokens` are subtracted from `input_tokens` for pricing
/// since cached tokens are billed at the (lower) cached rate.
pub fn estimate_cost(
    model_name: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
) -> f64 {
    let pricing = lookup_pricing(model_name);

    let uncached_input = input_tokens.saturating_sub(cached_input_tokens);
    (uncached_input as f64 * pricing.input)
        + (output_tokens as f64 * pricing.output)
        + (cached_input_tokens as f64 * pricing.cached_input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_sonnet_pricing() {
        // 1000 input + 500 output tokens on claude-sonnet-4
        let cost = estimate_cost("anthropic/claude-sonnet-4-20250514", 1000, 500, 0);
        // $3/M input + $15/M output = 0.003 + 0.0075 = 0.0105
        assert!((cost - 0.0105).abs() < 1e-10);
    }

    #[test]
    fn test_cached_tokens_reduce_cost() {
        let no_cache = estimate_cost("anthropic/claude-sonnet-4-20250514", 1000, 500, 0);
        let with_cache = estimate_cost("anthropic/claude-sonnet-4-20250514", 1000, 500, 500);
        assert!(with_cache < no_cache);
    }

    #[test]
    fn test_unknown_model_uses_fallback() {
        let cost = estimate_cost("unknown-provider/mystery-model", 1000, 500, 0);
        assert!(cost > 0.0);
    }
}

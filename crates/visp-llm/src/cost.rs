/// Anthropic 定价表：input $/MTok, output $/MTok
fn anthropic_rate(model: &str) -> (f64, f64) {
    // claude-sonnet-4-20250514 及其变体
    if model.contains("claude-sonnet-4")
        || model.contains("claude-sonnet-4-6")
        || model.contains("claude-sonnet-4-5")
    {
        (3.0, 15.0) // input $3/MTok, output $15/MTok
    } else if model.contains("claude-3-5-haiku") {
        (0.8, 4.0)
    } else if model.contains("claude-3-opus") {
        (15.0, 75.0)
    } else if model.contains("claude-3-haiku") {
        (0.25, 1.25)
    } else {
        (0.0, 0.0) // unknown model
    }
}

/// OpenAI 定价表：input $/MTok, output $/MTok
fn openai_rate(model: &str) -> (f64, f64) {
    if model.contains("gpt-4o") && model.contains("mini") {
        (0.15, 0.60) // gpt-4o-mini
    } else if model.contains("gpt-4o") {
        (2.5, 10.0) // gpt-4o
    } else if model.contains("gpt-4-turbo") {
        (10.0, 30.0)
    } else if model.contains("gpt-4") {
        (30.0, 60.0)
    } else if model.contains("gpt-3.5-turbo") || model.contains("gpt-35-turbo") {
        (0.5, 1.5)
    } else if model.contains("o1") {
        (15.0, 60.0)
    } else {
        (0.0, 0.0) // unknown model
    }
}

/// 从 Anthropic model 与 token 用量估算成本（美元）
pub fn anthropic_cost_usd(model: &str, input: u32, output: u32) -> f64 {
    let (input_rate, output_rate) = anthropic_rate(model);
    if input_rate == 0.0 && output_rate == 0.0 {
        return 0.0;
    }
    (input as f64 / 1_000_000.0 * input_rate) + (output as f64 / 1_000_000.0 * output_rate)
}

/// 从 OpenAI model 与 token 用量估算成本（美元）
pub fn openai_cost_usd(model: &str, input: u32, output: u32) -> f64 {
    let (input_rate, output_rate) = openai_rate(model);
    if input_rate == 0.0 && output_rate == 0.0 {
        return 0.0;
    }
    (input as f64 / 1_000_000.0 * input_rate) + (output as f64 / 1_000_000.0 * output_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_claude_sonnet_4_cost() {
        // claude-sonnet-4: input $3/MTok, output $15/MTok
        // 1000 input + 500 output tokens
        let cost = anthropic_cost_usd("claude-sonnet-4-20250514", 1000, 500);
        let expected = (1000.0 / 1_000_000.0 * 3.0) + (500.0 / 1_000_000.0 * 15.0);
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn test_anthropic_claude_sonnet_4_6_cost() {
        let cost = anthropic_cost_usd("claude-sonnet-4-6", 1_000_000, 0);
        assert!((cost - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_anthropic_unknown_model_returns_zero() {
        let cost = anthropic_cost_usd("unknown-model", 1000, 500);
        assert!((cost - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_openai_gpt4o_cost() {
        // gpt-4o: input $2.5/MTok, output $10/MTok
        let cost = openai_cost_usd("gpt-4o", 2000, 1000);
        let expected = (2000.0 / 1_000_000.0 * 2.5) + (1000.0 / 1_000_000.0 * 10.0);
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn test_openai_gpt4o_mini_cost() {
        let cost = openai_cost_usd("gpt-4o-mini", 1_000_000, 1_000_000);
        let expected = 0.15 + 0.60;
        assert!((cost - expected).abs() < 1e-10);
    }

    #[test]
    fn test_openai_unknown_model_returns_zero() {
        let cost = openai_cost_usd("unknown-model", 1000, 500);
        assert!((cost - 0.0).abs() < 1e-10);
    }
}

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
#[path = "cost_tests.rs"]
mod tests;

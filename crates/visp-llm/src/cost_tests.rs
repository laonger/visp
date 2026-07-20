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

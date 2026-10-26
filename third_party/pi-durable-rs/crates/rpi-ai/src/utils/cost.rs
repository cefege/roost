//! Port of `calculateCost` from `packages/ai/src/models.ts` @ pi 0.82.1
//! (2efa728), kept in `utils/` per design §3.6.
//!
//! Tier matching: `inputTokens = input + cacheRead + cacheWrite` (excludes
//! output); the highest satisfied `inputTokensAbove` threshold wins and its
//! full rate set applies request-wide. Anthropic 1h cache writes are charged
//! at 2x the base input rate (hard-coded).

use crate::types::{Model, ModelCostRates, Usage, UsageCost};

/// `calculateCost` — fills and returns `usage.cost`.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    calculate_cost_for(&model.cost, usage)
}

/// `calculateCost` against a bare `ModelCost` (schema-v6 image/classifier
/// entries carry the same cost shape).
pub fn calculate_cost_for(cost: &crate::types::ModelCost, usage: &mut Usage) -> UsageCost {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates: &ModelCostRates = &cost.rates;
    let mut matched_threshold: i64 = -1;
    for tier in cost.tiers.as_deref().unwrap_or(&[]) {
        if input_tokens > tier.input_tokens_above
            && tier.input_tokens_above as i64 > matched_threshold
        {
            rates = &tier.rates;
            matched_threshold = tier.input_tokens_above as i64;
        }
    }

    // Anthropic charges 2x base input for 1h cache writes.
    let long_write = usage.cache_write1h.unwrap_or(0);
    let short_write = usage.cache_write - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write = (rates.cache_write * short_write as f64
        + rates.input * 2.0 * long_write as f64)
        / 1_000_000.0;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost.clone()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn model(cost: serde_json::Value) -> Model {
        let mut value = json!({
            "id": "m", "name": "m", "api": "anthropic-messages", "provider": "anthropic",
            "baseUrl": "https://example.com", "reasoning": false, "input": ["text"],
            "contextWindow": 200000, "maxTokens": 8192,
        });
        value["cost"] = cost;
        serde_json::from_value(value).expect("model")
    }

    #[test]
    fn test_calculate_cost_flat_rates() {
        let model =
            model(json!({"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75}));
        let mut usage = Usage {
            input: 1_000_000,
            output: 100_000,
            cache_read: 500_000,
            cache_write: 200_000,
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        assert_eq!(cost.input, 3.0);
        assert_eq!(cost.output, 1.5);
        assert_eq!(cost.cache_read, 0.15);
        assert_eq!(cost.cache_write, 0.75);
        assert_eq!(cost.total, 5.4);
    }

    #[test]
    fn test_calculate_cost_tiers_highest_threshold_wins() {
        let model = model(json!({
            "input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 1.0,
            "tiers": [
                {"input": 10.0, "output": 20.0, "cacheRead": 1.0, "cacheWrite": 10.0, "inputTokensAbove": 100},
                {"input": 50.0, "output": 60.0, "cacheRead": 5.0, "cacheWrite": 50.0, "inputTokensAbove": 1000}
            ]
        }));
        let mut usage = Usage {
            input: 900,
            cache_read: 150,
            cache_write: 0,
            output: 10,
            ..Usage::default()
        };
        // inputTokens = 1050 → both thresholds satisfied, highest (1000) wins.
        let cost = calculate_cost(&model, &mut usage);
        assert!((cost.input - 50.0 * 900.0 / 1e6).abs() < 1e-12);
        assert!((cost.output - 60.0 * 10.0 / 1e6).abs() < 1e-12);
        assert!((cost.cache_read - 5.0 * 150.0 / 1e6).abs() < 1e-12);

        // inputTokens = 150 → only the first tier applies.
        let mut usage = Usage {
            input: 100,
            cache_read: 50,
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        assert!((cost.input - 10.0 * 100.0 / 1e6).abs() < 1e-12);

        // inputTokens below all thresholds → base rates.
        let mut usage = Usage {
            input: 50,
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        assert!((cost.input - 1.0 * 50.0 / 1e6).abs() < 1e-12);
    }

    #[test]
    fn test_calculate_cost_cache_write_1h_split() {
        let model =
            model(json!({"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75}));
        let mut usage = Usage {
            cache_write: 1000,
            cache_write1h: Some(400),
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        // short = 600 @ cacheWrite rate; long = 400 @ 2x input rate.
        let expected = (3.75 * 600.0 + 3.0 * 2.0 * 400.0) / 1e6;
        assert!((cost.cache_write - expected).abs() < 1e-12);
    }
}

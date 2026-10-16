//! OMP-specific model fact cells for the models hub rows and detail lines:
//! `400k ◫` context, `🧠 43` intelligence, `~118t/s` speed, and the declared
//! pricing state. Token counts and per-million cost legs come from
//! `hub_logic` (the one home for them); this module only adds OMP's wording.

use super::hub_logic::{format_cost_pair as cost_numbers, format_tokens};

/// `400k ◫`: the context window, empty when the model reports none.
pub fn format_context(context_window: u64) -> String {
    if context_window == 0 {
        return String::new();
    }
    format!("{} ◫", format_tokens(context_window))
}

/// `🧠 43`: the intelligence score, empty when the catalog has none.
pub fn format_intelligence(intelligence: Option<f32>) -> String {
    match intelligence {
        Some(score) if score.is_finite() => format!("🧠 {}", score.round() as i64),
        _ => String::new(),
    }
}

/// `~118t/s` (one decimal below 10 t/s). Empty when unknown or not positive.
pub fn format_tokens_per_second(tokens_per_second: Option<f32>) -> String {
    match tokens_per_second {
        Some(speed) if speed.is_finite() && speed > 0.0 => {
            if speed >= 10.0 {
                format!("~{}t/s", speed.round() as i64)
            } else {
                format!("~{speed:.1}t/s")
            }
        }
        _ => String::new(),
    }
}

/// The declared pricing state word, when the catalog states one.
fn declared_state(pricing_status: Option<&str>) -> Option<&'static str> {
    match pricing_status? {
        "free" => Some("free"),
        "included" => Some("included"),
        "variable" => Some("variable"),
        "unknown" => Some("unknown"),
        _ => None,
    }
}

/// The row's price cell: the declared state in one word, `free` when both
/// legs are zero, else `$in/out` per million tokens.
pub fn format_cost_pair(
    input_micros: u64,
    output_micros: u64,
    pricing_status: Option<&str>,
) -> String {
    if let Some(state) = declared_state(pricing_status) {
        return state.to_owned();
    }
    cost_numbers(input_micros, output_micros).unwrap_or_else(|| "free".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_uses_shared_token_counts() {
        assert_eq!(format_context(400_000), "400k ◫");
        assert_eq!(format_context(1_500_000), "1.5M ◫");
        assert_eq!(format_context(0), "");
    }

    #[test]
    fn cost_cell_names_free_and_declared_states() {
        assert_eq!(format_cost_pair(0, 0, None), "free");
        assert_eq!(format_cost_pair(3_000_000, 0, Some("included")), "included");
        assert_eq!(format_cost_pair(3_000_000, 15_000_000, None), "$3/15");
    }

    #[test]
    fn speed_and_intelligence_format() {
        assert_eq!(format_tokens_per_second(Some(118.4)), "~118t/s");
        assert_eq!(format_tokens_per_second(Some(4.25)), "~4.2t/s");
        assert_eq!(format_tokens_per_second(None), "");
        assert_eq!(format_intelligence(Some(42.6)), "🧠 43");
    }
}

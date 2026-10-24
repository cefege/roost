//! Context-window and spend projection for the conversation header.
//! The catalog supplies the effective model's window; transcript totals supply usage.
//! Unknown limits retain spend information without inventing a fill fraction.

use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderUsage {
    pub fraction: Option<f64>,
    pub title: String,
}

pub fn current_context_window(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> Option<u64> {
    let current = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref())?;
    catalog
        .and_then(|catalog| {
            catalog.models.iter().find(|model| {
                model.provider == current.provider && model.model_id == current.model_id
            })
        })
        .map(|model| model.context_window)
}

pub fn header_usage(
    context_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    context_window: Option<u64>,
) -> Option<HeaderUsage> {
    if total_tokens == 0 {
        return None;
    }
    let window = context_window.filter(|window| *window > 0);
    let fraction = window.map(|window| context_tokens as f64 / window as f64);
    let title = match window {
        Some(window) => format!(
            "{context_tokens} of {window} context tokens ({:.0}%) · {total_tokens} total this session · ${cost_usd:.4} spent",
            fraction.unwrap_or(0.0) * 100.0
        ),
        None => format!("{total_tokens} tokens this session · ${cost_usd:.4} spent"),
    };
    Some(HeaderUsage { fraction, title })
}

#[cfg(test)]
mod tests {
    use super::header_usage;

    #[test]
    fn nothing_spent_yet_leaves_the_header_bare() {
        assert_eq!(header_usage(0, 0, 0.0, Some(200_000)), None);
    }

    #[test]
    fn a_known_window_gives_the_rings_fill_from_the_latest_call_only() {
        let Some(usage) = header_usage(40_000, 140_000, 0.0123, Some(200_000)) else {
            panic!("spent tokens produce header usage");
        };
        assert!(
            usage
                .fraction
                .is_some_and(|fraction| (fraction - 0.2).abs() < 1e-9)
        );
        assert!(usage.title.contains("40000 of 200000"));
        assert!(usage.title.contains("20%"));
        assert!(usage.title.contains("140000 total"));
        assert!(usage.title.contains("$0.0123"));
    }

    #[test]
    fn an_unknown_window_still_reports_spend_with_no_ring_fill() {
        let Some(usage) = header_usage(40_000, 40_000, 0.5, None) else {
            panic!("spent tokens produce header usage");
        };
        assert_eq!(usage.fraction, None);
        assert!(usage.title.contains("40000 tokens"));
    }
}

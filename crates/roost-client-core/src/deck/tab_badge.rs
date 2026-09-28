//! The compact deck bar's terminal-count badge: "position/total" over the
//! folder's flattened terminal order (the order a swipe walks), or the bare
//! total when the painted terminal is not in the list. Read by the web
//! `MobileDeckBar`. Pure; ports `apps/web/src/lib/deckTabBadge.ts`.

/// What the count square shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckTabBadge {
    /// The glyphs inside the badge: "3/5", or the bare total.
    pub text: String,
    /// The sentence for `aria-label` and `title`.
    pub description: String,
    /// Whether `text` is the fraction, which widens the badge.
    pub fraction: bool,
}

/// The badge for `tab_count` terminals with the painted one at
/// `active_index` (0-based), or `None` when it is not in the list.
pub fn deck_tab_badge(tab_count: usize, active_index: Option<usize>) -> DeckTabBadge {
    match active_index {
        Some(index) if tab_count > 1 && index < tab_count => {
            let position = index + 1;
            DeckTabBadge {
                text: format!("{position}/{tab_count}"),
                description: format!("terminal {position} of {tab_count} in this workspace"),
                fraction: true,
            }
        }
        _ => DeckTabBadge {
            text: tab_count.to_string(),
            description: format!(
                "{tab_count} terminal{} in this workspace",
                if tab_count == 1 { "" } else { "s" }
            ),
            fraction: false,
        },
    }
}

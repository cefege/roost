//! The hub's footer legend and status line, the words the dialog shows for the
//! current view. Both are pure functions of the view and focus so they stay testable.

use roost_protocol::wire::agent_chat::ModelsCatalog;

use super::hub_keys::HubFocus;
use super::hub_logic::HubView;

pub(super) fn footer_hints(view: &HubView, locked: bool, focus: HubFocus) -> String {
    if locked {
        return "⏎ log in · ↑↓ providers · ⎋ close".to_owned();
    }
    if matches!(view, HubView::Roles) {
        return "⏎ pick · f fallback · x clear · t thinking · [ ] reorder · s save preset · ⎋ close".to_owned();
    }
    if matches!(view, HubView::ThisChat) {
        return if focus == HubFocus::List {
            "⏎ use for this chat · ↑↓ models · ⎋ close"
        } else {
            "⏎ models · ↑↓ views · ⎋ close"
        }
        .to_owned();
    }
    match focus {
        HubFocus::Scope => {
            "⏎/→ models · ↑↓ providers · type to search · ⌥←/⌥→ kind · ⎋ close".to_owned()
        }
        HubFocus::List => {
            "⏎ assign roles · ↑↓ models · ← providers · type to search · ⌥←/⌥→ kind · ⎋ close"
                .to_owned()
        }
    }
}

pub(super) fn status_text(view: &HubView, catalog: &ModelsCatalog) -> String {
    match view {
        HubView::ThisChat => "Enter switches only this chat".to_owned(),
        HubView::Recent => "Recently used models".to_owned(),
        HubView::Provider(id) => match catalog.providers.iter().find(|provider| provider.id == *id)
        {
            Some(provider) if provider.configured => format!("{} · models", provider.name),
            Some(provider) => format!("{} · not configured", provider.name),
            None => "Provider not found".to_owned(),
        },
        _ => "All available models".to_owned(),
    }
}

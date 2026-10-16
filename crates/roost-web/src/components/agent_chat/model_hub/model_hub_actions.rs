//! What the hub does with a model row and its role strip. Enter and a click on
//! a row both call `activate_row`, which reports the row whose strip opens. A
//! chip saves one chat role in the strip's scope, or for "This chat" switches
//! this chat's model and closes the hub. Locked providers still hand off to
//! sign-in, and web rows have no strip.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, ModelsCatalog, ProviderEntry,
};

use super::hub_browse::BrowseRow;
use super::hub_logic::HubView;
use super::model_role_strip::OpenStrip;
use super::strip_chips::{StripAction, StripChip, apply_settings, strip_view_chips};
use super::strip_thinking::thinking_step_after_assign;
use crate::components::agent_chat::model_controls::{Configure, configure};
use crate::components::agent_chat::role_switch::{initial_scope, pin_clears_for};
use crate::components::settings::agent_roles::save_settings;
use crate::components::settings::model_scope::ModelScope;
use crate::pump::Pump;

/// Everything a strip chip needs to save and close: the chat, its pin, and the
/// signals the hub reads.
#[derive(Clone)]
pub(super) struct StripCommit {
    pub pump: Pump,
    pub conversation_id: String,
    pub pinned: bool,
    pub running: &'static str,
    pub settings: Signal<AgentSettingsView>,
    pub error: Signal<String>,
    pub open: Signal<bool>,
    pub strip: Signal<Option<OpenStrip>>,
}

/// Activates the row at `index`: a locked provider starts its sign-in and closes
/// the hub, a web row does nothing. Returns the selector of a row whose strip
/// should open.
pub fn activate_row(
    view: &HubView,
    catalog: &ModelsCatalog,
    rows: &[BrowseRow],
    index: usize,
    open: &mut Signal<bool>,
    connect_provider: &mut Signal<Option<ProviderEntry>>,
) -> Option<String> {
    if let HubView::Provider(provider_id) = view
        && !catalog
            .providers
            .iter()
            .any(|provider| provider.id == *provider_id && provider.configured)
    {
        if let Some(entry) = catalog
            .providers
            .iter()
            .find(|provider| provider.id == *provider_id)
        {
            connect_provider.set(Some(entry.clone()));
        }
        open.set(false);
        return None;
    }
    let row = rows.get(index)?;
    if row.provider == "web" {
        return None;
    }
    Some(row.selector.clone())
}

/// The folder a conversation runs in: the scope the strip's "This folder" chip writes.
pub(super) fn conversation_folder(conversation: &ConversationSummary) -> ModelScope {
    ModelScope::Folder {
        worker_fp: conversation.worker_fp.clone(),
        cwd: conversation.cwd.clone(),
    }
}

/// The strip for `selector`, scoped the way the default role starts: this folder
/// when the folder already overrides it, else all folders.
pub(super) fn opened_strip(
    settings: &AgentSettingsView,
    folder: &ModelScope,
    selector: String,
) -> OpenStrip {
    OpenStrip {
        scope: initial_scope(settings, folder, "default"),
        selector,
        cursor: 0,
        thinking_role: None,
    }
}

/// Changes the scope of the open strip. The selector and cursor stay put.
pub(super) fn rescope_strip(mut strip: Signal<Option<OpenStrip>>, scope: ModelScope) {
    if let Some(open_strip) = strip() {
        strip.set(Some(OpenStrip {
            scope,
            ..open_strip
        }));
    }
}

/// Applies a strip chip to the row's model. A role chip saves that role in the
/// strip's scope; assigning a model that takes thinking levels then opens the
/// thinking step, and an edit to the running role clears a pin it hides. The
/// strip closes unless a thinking step replaces it.
pub(super) fn apply_strip_chip(
    commit: &StripCommit,
    catalog: &ModelsCatalog,
    open_strip: &OpenStrip,
    chip: &StripChip,
) {
    let mut strip = commit.strip;
    strip.set(None);
    let selector = open_strip.selector.as_str();
    let Some(role) = chip.action.role() else {
        let mut open = commit.open;
        configure(
            commit.pump.clone(),
            commit.conversation_id.clone(),
            Configure::Model(Some(selector.to_owned())),
        );
        open.set(false);
        return;
    };
    let before = (commit.settings)();
    let Some(updated) = apply_settings(&before, &chip.action, selector, &open_strip.scope, catalog)
    else {
        return;
    };
    save_settings(commit.pump.clone(), updated, commit.settings, commit.error);
    if !matches!(chip.action, StripAction::Think { .. })
        && pin_clears_for(commit.pinned, commit.running, &[role])
    {
        configure(
            commit.pump.clone(),
            commit.conversation_id.clone(),
            Configure::Model(None),
        );
    }
    if matches!(chip.action, StripAction::Assign(_))
        && let Some(step) =
            thinking_step_after_assign(&before, catalog, selector, &open_strip.scope, role)
    {
        strip.set(Some(step));
    }
}

/// Applies the strip chip at `chip_index` against the strip as it stands now,
/// so a click reads the same chips the strip rendered.
pub(super) fn activate_strip_chip(
    commit: &StripCommit,
    catalog: &ModelsCatalog,
    current: Option<&str>,
    chip_index: usize,
) {
    let Some(open_now) = (commit.strip)() else {
        return;
    };
    let chips_now = strip_view_chips(&(commit.settings)(), catalog, &open_now, current);
    if let Some(chip) = chips_now.get(chip_index) {
        apply_strip_chip(commit, catalog, &open_now, chip);
    }
}

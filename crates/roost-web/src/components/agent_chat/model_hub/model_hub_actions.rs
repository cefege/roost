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
use super::hub_keys::HubKeyEffect;
use super::hub_logic::HubSection;
use super::hub_logic::HubView;
use super::model_role_strip::OpenStrip;
use super::strip_chips::{StripAction, StripChip, apply_settings, strip_view_chips};
use super::strip_thinking::thinking_step_after_assign;
use crate::components::agent_chat::model_controls::configure_model_pin;
use crate::components::agent_chat::role_switch::{initial_scope, pin_clears_for};
use crate::components::settings::agent_roles::save_settings;
use crate::components::settings::model_scope::ModelScope;
use crate::pump::Pump;

/// The hub frame's id. A listbox row carries `tabindex` so a click can focus
/// it, and that element can disappear on the next render (a view switch, a
/// re-filtered row list); the frame holds the key handler, so a click or key
/// effect must pull focus back to it or the next key reaches nothing.
pub(super) const HUB_ROOT_ID: &str = "agent-model-hub-root";

pub(super) fn refocus_frame() {
    crate::components::file_viewer::dom::focus_by_id(HUB_ROOT_ID);
}

/// The signals `on_key`'s match writes. Bundled so the whole effect ladder
/// lives in one function instead of the frame component.
#[derive(Clone)]
pub(super) struct HubKeySignals {
    pub view: Signal<HubView>,
    pub kind: Signal<String>,
    pub query: Signal<String>,
    pub selected: Signal<usize>,
    pub focus_pane: Signal<super::hub_keys::HubFocus>,
    pub open: Signal<bool>,
    pub connect_provider: Signal<Option<ProviderEntry>>,
    pub strip: Signal<Option<OpenStrip>>,
}

/// The read-only state `apply_key_effect` needs: the sections and kinds a
/// move wraps over, the row data a row-effect reads, and where a newly
/// opened strip is scoped.
#[derive(Clone, Copy)]
pub(super) struct HubKeyData<'a> {
    pub sections: &'a [HubSection],
    pub kinds: &'a [String],
    pub catalog: &'a ModelsCatalog,
    pub rows: &'a [BrowseRow],
    pub settings: &'a AgentSettingsView,
    pub folder: &'a ModelScope,
    pub index: usize,
    pub row_count: usize,
}

/// Applies one `HubKeyEffect` to the hub's state: moves the view or row
/// cursor, edits the search query, activates a row, or closes the hub. A
/// view or kind change refocuses the frame, since it replaces the row
/// listbox a click may have focused; row movement and query edits must not,
/// since the search field can still hold real DOM focus for those.
pub(super) fn apply_key_effect(
    effect: HubKeyEffect,
    signals: &mut HubKeySignals,
    data: &HubKeyData<'_>,
) {
    let HubKeyData {
        sections,
        kinds,
        catalog,
        rows,
        settings,
        folder,
        index,
        row_count,
    } = *data;
    match effect {
        HubKeyEffect::MoveView(step) => {
            let position = sections
                .iter()
                .position(|section| section.view == signals.view.peek().clone())
                .unwrap_or(0);
            let next = super::hub_catalog::wrap_index(position, step, sections.len());
            signals.view.set(sections[next].view.clone());
            signals.selected.set(0);
            refocus_frame();
        }
        HubKeyEffect::MoveRow(step) => {
            signals
                .selected
                .set(super::hub_catalog::wrap_index(index, step, row_count));
        }
        HubKeyEffect::PageRow(step) => {
            let target = index as i64 + i64::from(step) * 8;
            signals
                .selected
                .set(target.clamp(0, row_count.saturating_sub(1) as i64) as usize);
        }
        HubKeyEffect::JumpRow { to_end } => {
            signals.selected.set(if to_end {
                row_count.saturating_sub(1)
            } else {
                0
            });
        }
        HubKeyEffect::MoveKind(step) => {
            let active = signals.kind.peek().clone();
            let position = kinds.iter().position(|name| *name == active).unwrap_or(0);
            if let Some(next) =
                kinds.get(super::hub_catalog::wrap_index(position, step, kinds.len()))
            {
                signals.kind.set(next.clone());
            }
            signals.selected.set(0);
            refocus_frame();
        }
        HubKeyEffect::ActivateRow => {
            if let Some(selector) = activate_row(
                &signals.view.peek(),
                catalog,
                rows,
                index,
                &mut signals.open,
                &mut signals.connect_provider,
            ) {
                signals
                    .strip
                    .set(Some(opened_strip(settings, folder, selector)));
            }
        }
        HubKeyEffect::FocusList => signals.focus_pane.set(super::hub_keys::HubFocus::List),
        HubKeyEffect::FocusView => signals.focus_pane.set(super::hub_keys::HubFocus::Scope),
        HubKeyEffect::ClearQuery => {
            signals.query.set(String::new());
            signals.selected.set(0);
        }
        HubKeyEffect::Backspace => {
            let mut next = signals.query.peek().clone();
            next.pop();
            signals.query.set(next);
            signals.selected.set(0);
        }
        HubKeyEffect::Search(letter) => {
            let mut next = signals.query.peek().clone();
            next.push(letter);
            signals.query.set(next);
            signals.selected.set(0);
        }
        HubKeyEffect::RefreshProvider | HubKeyEffect::Ignore => {}
        HubKeyEffect::Close => signals.open.set(false),
    }
}

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
        configure_model_pin(
            commit.pump.clone(),
            commit.conversation_id.clone(),
            Some(selector.to_owned()),
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
        configure_model_pin(commit.pump.clone(), commit.conversation_id.clone(), None);
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

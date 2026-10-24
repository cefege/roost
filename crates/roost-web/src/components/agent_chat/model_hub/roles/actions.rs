//! The Roles view's actions: each `RoleAction` (a key or a click) reads the saved
//! settings, runs one `edits` operation on the row under the cursor, and saves the
//! result. Owns the cursor, the open picker and the preset-name field, and maps
//! keys to actions, so the component only renders and forwards events.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ModelsCatalog, presets_with_builtins};

use super::edits::{
    active_preset_name, add_fallback, assign_entry, assign_leader, clear_role, cycle_preset,
    move_fallback, next_thinking, remove_fallback, set_entry_thinking, split_entry,
};
use super::rows::{RoleTab, RolesRow, build_rows, chain_of};
use crate::components::agent_chat::model_controls::{Configure, configure};
use crate::components::agent_chat::role_switch::{
    pin_clears_for, thinking_choices, with_applied_preset, with_saved_preset,
};
use crate::components::file_viewer::dom::focus_by_id;
use crate::components::settings::agent_roles::save_settings;
use crate::pump::Pump;

/// The root element's id, focused so the keys reach the view.
pub(super) const ROOT_ID: &str = "agent-model-hub-roles";
/// The preset-name field's id, focused when naming opens.
pub(super) const NAME_FIELD_ID: &str = "agent-hub-roles-preset-name";

/// Where a picked model lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PickTarget {
    /// The role's leader.
    Leader(String),
    /// The chain entry at `index` (≥ 1).
    Entry { role: String, index: usize },
    /// A new fallback appended to `role`.
    Add(String),
}

impl PickTarget {
    pub(super) fn role(&self) -> &str {
        match self {
            PickTarget::Leader(role) | PickTarget::Add(role) => role,
            PickTarget::Entry { role, .. } => role,
        }
    }

    /// The model the picker starts on: the leader or entry it replaces.
    pub(super) fn current_selector(&self, settings: &AgentSettingsView) -> Option<String> {
        let index = match self {
            PickTarget::Leader(_) => 0,
            PickTarget::Entry { index, .. } => *index,
            PickTarget::Add(_) => return None,
        };
        chain_of(settings, self.role()).into_iter().nth(index)
    }
}

/// One key or click on the Roles list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RoleAction {
    /// Activate the row at this position (a click, or ⏎ on the cursor row).
    Activate(usize),
    /// Move the cursor by one visible row.
    Step(isize),
    /// `f`: add a fallback to the cursor's role.
    Fallback,
    /// `x`: clear the role, or remove the fallback under the cursor.
    Clear,
    /// `t`: cycle the thinking level of the leader or fallback under the cursor.
    Thinking,
    /// `[` / `]`: move the fallback under the cursor.
    Move(isize),
    /// `p` / `P` and ctrl+←/→: apply the preset `delta` steps away.
    Preset(isize),
    /// alt+←/→: switch the tab.
    Tab(isize),
    /// `s`: open the preset-name field.
    OpenNaming,
}

/// Everything an action reads and writes. Each event handler holds a clone.
#[derive(Clone)]
pub(super) struct RoleContext {
    pub catalog: ModelsCatalog,
    pub settings: Signal<AgentSettingsView>,
    pub error: Signal<String>,
    pub pump: Pump,
    pub conversation_id: String,
    pub running: &'static str,
    /// Whether the chat pins its own model; only a pin needs clearing.
    pub pinned: bool,
    pub tab: Signal<RoleTab>,
    pub cursor: Signal<usize>,
    pub anchor: Signal<String>,
    pub picking: Signal<Option<PickTarget>>,
    pub naming: Signal<bool>,
    pub preset_name: Signal<String>,
}

/// Maps a key to its action. Escape closes the open picker or the naming field
/// and is consumed only then; other keys pass through while either is open.
pub(super) fn handle_key(context: &RoleContext, event: &KeyboardEvent) {
    let picker_open = context.picking.peek().is_some();
    let naming = *context.naming.peek();
    if event.key() == Key::Named(NamedKey::Escape) {
        if picker_open {
            event.prevent_default();
            event.stop_propagation();
            context.close_picker();
        } else if naming {
            event.prevent_default();
            event.stop_propagation();
            context.cancel_naming();
        }
        return;
    }
    if picker_open || naming {
        return;
    }
    let modifiers = event.modifiers();
    let ctrl = modifiers.contains(Modifiers::CONTROL);
    let alt = modifiers.contains(Modifiers::ALT);
    let meta = modifiers.contains(Modifiers::META);
    let action = match event.key() {
        Key::Named(NamedKey::ArrowDown) if !ctrl && !alt => Some(RoleAction::Step(1)),
        Key::Named(NamedKey::ArrowUp) if !ctrl && !alt => Some(RoleAction::Step(-1)),
        Key::Named(NamedKey::ArrowLeft) if ctrl => Some(RoleAction::Preset(-1)),
        Key::Named(NamedKey::ArrowRight) if ctrl => Some(RoleAction::Preset(1)),
        Key::Named(NamedKey::ArrowLeft) if alt => Some(RoleAction::Tab(-1)),
        Key::Named(NamedKey::ArrowRight) if alt => Some(RoleAction::Tab(1)),
        Key::Named(NamedKey::Enter) => Some(RoleAction::Activate(*context.cursor.peek())),
        Key::Character(text) if !ctrl && !alt && !meta => match text.as_str() {
            "f" => Some(RoleAction::Fallback),
            "x" => Some(RoleAction::Clear),
            "t" => Some(RoleAction::Thinking),
            "[" => Some(RoleAction::Move(-1)),
            "]" => Some(RoleAction::Move(1)),
            "s" => Some(RoleAction::OpenNaming),
            "p" => Some(RoleAction::Preset(1)),
            "P" => Some(RoleAction::Preset(-1)),
            _ => None,
        },
        _ => None,
    };
    if let Some(action) = action {
        event.prevent_default();
        context.run(action);
    }
}

impl RoleContext {
    pub(super) fn run(&self, action: RoleAction) {
        let settings = self.settings.peek().clone();
        let rows = build_rows(&settings, *self.tab.peek());
        let cursor = (*self.cursor.peek()).min(rows.len().saturating_sub(1));
        match action {
            RoleAction::Activate(position) => {
                self.move_cursor_to(position, &rows);
                if let Some(row) = rows.get(position) {
                    self.activate(row);
                }
            }
            RoleAction::Step(delta) => {
                let next = step_cursor(&rows, cursor, delta);
                self.move_cursor_to(next, &rows);
            }
            RoleAction::Fallback => {
                if let Some(role) = self.role_at(&rows, cursor) {
                    self.open_picker(PickTarget::Add(role));
                }
            }
            RoleAction::Clear => match rows.get(cursor) {
                Some(RolesRow::Role { role }) => {
                    self.commit(Some(clear_role(&settings, role)), &[*role])
                }
                Some(RolesRow::Fallback { role, index, .. }) => {
                    self.commit(remove_fallback(&settings, role, *index), &[*role]);
                }
                _ => {}
            },
            RoleAction::Thinking => self.cycle_thinking(&settings, rows.get(cursor)),
            RoleAction::Move(step) => {
                if let Some(RolesRow::Fallback { role, index, .. }) = rows.get(cursor) {
                    self.commit(move_fallback(&settings, role, *index, step), &[*role]);
                }
            }
            RoleAction::Preset(delta) => {
                if let Some(name) = cycle_preset(&settings, delta) {
                    self.apply_named_preset(&name);
                }
            }
            RoleAction::Tab(delta) => {
                let mut tab = self.tab;
                let next = self.tab.peek().step(delta);
                tab.set(next);
                self.move_cursor_to(0, &build_rows(&settings, next));
            }
            RoleAction::OpenNaming => {
                let mut naming = self.naming;
                naming.set(true);
            }
        }
    }

    /// Applies the named preset, unless it is already the active one.
    pub(super) fn apply_named_preset(&self, name: &str) {
        let settings = self.settings.peek().clone();
        if active_preset_name(&settings).as_deref() == Some(name) {
            return;
        }
        let Some(roles) = presets_with_builtins(&settings.model_presets).remove(name) else {
            return;
        };
        let touched: Vec<&str> = roles.keys().map(String::as_str).collect();
        self.commit(Some(with_applied_preset(&settings, &roles)), &touched);
    }

    /// Picks a model for `target` and saves it, closing the picker.
    pub(super) fn choose(&self, target: &PickTarget, selector: &str) {
        let settings = self.settings.peek().clone();
        let next = match target {
            PickTarget::Leader(role) => Some(assign_leader(&settings, role, selector)),
            PickTarget::Entry { role, index } => assign_entry(&settings, role, *index, selector),
            PickTarget::Add(role) => Some(add_fallback(&settings, role, selector)),
        };
        self.commit(next, &[target.role()]);
        self.close_picker();
    }

    /// Saves the current roles as the named preset, replacing one of that name.
    pub(super) fn save_preset(&self, name: &str) {
        let settings = self.settings.peek().clone();
        match with_saved_preset(&settings, name) {
            Ok(next) => {
                self.commit(Some(next), &[]);
                self.cancel_naming();
            }
            Err(message) => {
                let mut error = self.error;
                error.set(message);
            }
        }
    }

    pub(super) fn close_picker(&self) {
        let mut picking = self.picking;
        picking.set(None);
        focus_by_id(ROOT_ID);
    }

    pub(super) fn cancel_naming(&self) {
        let mut naming = self.naming;
        let mut preset_name = self.preset_name;
        naming.set(false);
        preset_name.set(String::new());
    }

    fn activate(&self, row: &RolesRow) {
        match row {
            RolesRow::Role { role } => self.open_picker(PickTarget::Leader((*role).to_owned())),
            RolesRow::Fallback { role, index, .. } => {
                self.open_picker(PickTarget::Entry {
                    role: (*role).to_owned(),
                    index: *index,
                });
            }
            RolesRow::NewFallback => {
                let anchor = self.anchor.peek().clone();
                self.open_picker(PickTarget::Add(anchor));
            }
            RolesRow::Separator => {}
        }
    }

    fn open_picker(&self, target: PickTarget) {
        let mut picking = self.picking;
        picking.set(Some(target));
    }

    /// The role `f` and `New fallback` act on: the row's own role, else the anchor.
    fn role_at(&self, rows: &[RolesRow], cursor: usize) -> Option<String> {
        match rows.get(cursor) {
            Some(RolesRow::Role { role }) | Some(RolesRow::Fallback { role, .. }) => {
                Some((*role).to_owned())
            }
            Some(RolesRow::NewFallback) => Some(self.anchor.peek().clone()),
            _ => None,
        }
    }

    fn move_cursor_to(&self, position: usize, rows: &[RolesRow]) {
        let mut cursor = self.cursor;
        cursor.set(position);
        if let Some(RolesRow::Role { role } | RolesRow::Fallback { role, .. }) = rows.get(position)
        {
            let mut anchor = self.anchor;
            anchor.set((*role).to_owned());
        }
    }

    fn cycle_thinking(&self, settings: &AgentSettingsView, row: Option<&RolesRow>) {
        let Some((role, index)) = (match row {
            Some(RolesRow::Role { role }) => Some((*role, 0)),
            Some(RolesRow::Fallback { role, index, .. }) => Some((*role, *index)),
            _ => None,
        }) else {
            return;
        };
        let Some(entry) = chain_of(settings, role).into_iter().nth(index) else {
            return;
        };
        let (base, level) = split_entry(&self.catalog, &entry);
        let choices = thinking_choices(&self.catalog, base);
        if choices.is_empty() {
            return;
        }
        let next = next_thinking(level, &choices);
        self.commit(
            set_entry_thinking(settings, &self.catalog, role, index, next.as_deref()),
            &[role],
        );
    }

    /// A session pin holds this chat to its own model, so an edit to the role the
    /// chat runs clears it, as the composer does.
    fn clear_pin_if_hidden(&self, touched: &[&str]) {
        if pin_clears_for(self.pinned, self.running, touched) {
            configure(
                self.pump.clone(),
                self.conversation_id.clone(),
                Configure::Model(None),
            );
        }
    }

    fn commit(&self, next: Option<AgentSettingsView>, touched: &[&str]) {
        if let Some(next) = next {
            save_settings(self.pump.clone(), next, self.settings, self.error);
            self.clear_pin_if_hidden(touched);
        }
    }
}

/// The next row `delta` steps from `cursor` that is not a separator, wrapping.
fn step_cursor(rows: &[RolesRow], cursor: usize, delta: isize) -> usize {
    let count = rows.len() as isize;
    if count == 0 {
        return 0;
    }
    let mut position = cursor as isize;
    for _ in 0..rows.len() {
        position = (position + delta).rem_euclid(count);
        if !matches!(rows[position as usize], RolesRow::Separator) {
            break;
        }
    }
    position as usize
}

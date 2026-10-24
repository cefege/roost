//! The role strip a model row opens: one chip per chat role plus "This chat",
//! a cursor that ←/→ move, an "All folders" / "This folder" scope toggle, and
//! Enter applies the chip under the cursor. Ported from OMP `model-hub.ts`'s
//! strip state; the chips' writes are in `model_hub_actions`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::ModelsCatalog;

use super::hub_catalog::wrap_index;
use super::model_hub_actions::{StripCommit, apply_strip_chip};
use super::strip_chips::{StripChip, StripKey};
use crate::components::settings::model_scope::ModelScope;
use crate::components::ui::{Card, CardContent, CardSize, Chip};

/// The footer legend while the strip is open.
pub(super) const STRIP_FOOTER: &str = "←/→ pick · ⏎ apply · ⎋ back to models";

/// The test id on the strip's container. Formatted rather than literal so the
/// attribute is a dynamic mutation the mount harness can observe.
const STRIP_TEST_ID: &str = "agent-model-hub-strip";

/// An open strip: the row's selector, the chip under the cursor, and the scope
/// its chips read and write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenStrip {
    pub selector: String,
    pub cursor: usize,
    pub scope: ModelScope,
    /// The role whose thinking step is open in place of the role chips.
    pub thinking_role: Option<&'static str>,
}

/// Applies one key the open strip consumes. Escape only closes the strip. Enter
/// applies the chip under the cursor; a role chip may open the thinking step in
/// its place.
pub(super) fn run_strip_key(
    key: StripKey,
    open_strip: &OpenStrip,
    chips: &[StripChip],
    catalog: &ModelsCatalog,
    commit: &StripCommit,
    mut strip: Signal<Option<OpenStrip>>,
) {
    match key {
        StripKey::Move(step) => strip.set(Some(OpenStrip {
            cursor: wrap_index(open_strip.cursor, step, chips.len()),
            ..open_strip.clone()
        })),
        StripKey::Apply => {
            if let Some(chip) = chips.get(open_strip.cursor) {
                apply_strip_chip(commit, catalog, open_strip, chip);
            }
        }
        StripKey::Close => strip.set(None),
    }
}

/// The role chips for the row's model and the scope toggle above them. A click
/// or Enter applies a chip; the cursor marks the chip Enter would apply.
#[component]
pub(super) fn ModelRoleStrip(
    chips: Vec<StripChip>,
    cursor: usize,
    scope: ModelScope,
    folder: ModelScope,
    on_activate: EventHandler<usize>,
    on_scope: EventHandler<ModelScope>,
) -> Element {
    let on_folder = scope == folder;
    rsx! {
        Card {
            size: CardSize::Sm,
            class: "gap-2",
            role: "group",
            "aria-label": "Assign this model to a role",
            "data-testid": "{STRIP_TEST_ID}",
            CardContent { class: "flex flex-col gap-2",
                div {
                    class: "flex flex-wrap items-center gap-2",
                    role: "group",
                    "aria-label": "Role scope",
                    Chip {
                        label: "All folders".to_owned(),
                        selected: Some(!on_folder),
                        test_id: "agent-model-hub-strip-scope-global".to_owned(),
                        onclick: move |_| on_scope.call(ModelScope::Global),
                    }
                    Chip {
                        label: "This folder".to_owned(),
                        selected: Some(on_folder),
                        test_id: "agent-model-hub-strip-scope-folder".to_owned(),
                        onclick: move |_| on_scope.call(folder.clone()),
                    }
                }
                for (index, chip) in chips.iter().enumerate() {
                    Chip {
                        key: "{index}",
                        label: chip.label.clone(),
                        selected: Some(chip.assigned),
                        test_id: format!("agent-model-hub-strip-chip-{index}"),
                        class: if index == cursor { "ring-2 ring-ring ring-offset-2 ring-offset-background" } else { "" },
                        onclick: move |_| on_activate.call(index),
                    }
                }
            }
        }
    }
}

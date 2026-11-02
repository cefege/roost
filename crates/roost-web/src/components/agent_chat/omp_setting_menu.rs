//! One OMP conversation-pin dropdown, including explicit inheritance.
//! Model and thinking controls use identical menu and source-label recipes.
//! Choices are machine-reported; an existing pin remains selectable offline.

use super::omp_controls::{choice_label, source_label};
use crate::components::ui::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{OmpChoice, OmpEffectiveValue};

#[component]
pub(super) fn OmpSettingMenu(
    label: &'static str,
    value: OmpEffectiveValue,
    inherited: OmpEffectiveValue,
    choices: Vec<OmpChoice>,
    disabled: bool,
    on_open: EventHandler<()>,
    on_choose: EventHandler<Option<String>>,
) -> Element {
    let mut open = use_signal(|| false);
    let source = source_label(&value.source);
    let current = value.value.as_ref().map_or_else(
        || format!("Choose {}", label.to_ascii_lowercase()),
        |value| format!("{label}: {value} · {source}"),
    );
    let inherited_label = format!(
        "Inherit · {} ({})",
        inherited.value.as_deref().unwrap_or("OMP's own config"),
        source_label(&inherited.source)
    );
    rsx! {
        DropdownMenu {open:open(),on_open_change:move |next| {open.set(next);if next {on_open.call(());}},
            DropdownMenuTrigger {ghost:true,disabled,aria_label:format!("OMP {label}"),title:current.clone(),class:"max-w-48",
                span {class:"truncate", "{current}"}
            }
            DropdownMenuContent {class:"max-h-80 w-72 max-w-full overflow-y-auto",
                DropdownMenuItem {onclick:move |_| on_choose.call(None), "{inherited_label}"}
                for choice in choices {
                    {
                        let shown = choice_label(&choice);
                        rsx! { DropdownMenuItem {key:"{choice.value}",title:choice.description.clone(),onclick:move |_| on_choose.call(Some(choice.value.clone())), "{shown}"} }
                    }
                }
            }
        }
    }
}

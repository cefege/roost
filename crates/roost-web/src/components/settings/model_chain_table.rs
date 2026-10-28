//! A compact priority table for ordered model and search-engine selectors.
//! The editor owns mutations; keyed row components preserve their controls
//! across reorders while reporting committed actions back to the editor.

use dioxus::prelude::*;

use crate::components::agent_chat::model_picker::PickerOption;
use crate::components::settings::model_chain::split_thinking;
use crate::components::ui::{Table, TableBody, TableHead, TableHeader, TableRow, TableWrapper};

mod row;
use row::ModelChainRow;

/// Displays the ordered candidates for one role without normalizing them.
#[component]
pub fn ModelChainTable(
    role_title: String,
    chain: Vec<String>,
    options: Vec<PickerOption>,
    thinking_levels: Vec<String>,
    test_id: String,
    #[props(default)] engines: bool,
    on_edit: EventHandler<usize>,
    on_reorder: EventHandler<(usize, usize)>,
    on_remove: EventHandler<usize>,
) -> Element {
    let dragged_row = use_signal(|| None::<usize>);
    let row_count = chain.len();
    let options_available = !options.is_empty();
    let mut occurrences = std::collections::BTreeMap::<&str, usize>::new();
    let rows = chain.iter().enumerate().map(|(index, token)| {
        let occurrence = occurrences.entry(token.as_str()).or_default();
        let duplicate_ordinal = *occurrence;
        *occurrence += 1;
        let (base, thinking) = split_thinking(token, &thinking_levels);
        let option = options.iter().find(|option| option.selector == base);
        let model_name = option.map_or_else(|| base.to_owned(), |option| option.name.clone());
        let unavailable = option.is_none() && !base.starts_with('@');
        (
            index,
            token,
            duplicate_ordinal,
            model_name,
            thinking.map(str::to_owned),
            unavailable,
        )
    });
    rsx! {
        div {
            role: "group",
            "aria-label": "{role_title} model priority",
            "data-testid": "{test_id}",
            TableWrapper { class: "w-full max-w-full overflow-x-auto",
                Table { class: "w-full min-w-md table-fixed",
                    TableHeader {
                        TableRow {
                            TableHead { class: "w-32", "Priority" }
                            TableHead { "Model" }
                            TableHead { class: "w-20 text-right",
                                span { class: "sr-only", "Actions" }
                            }
                        }
                    }
                    TableBody {
                        for (index, token, ordinal, model_name, thinking, unavailable) in rows {
                            ModelChainRow {
                                key: "{token}-{ordinal}",
                                token: token.clone(),
                                model_name,
                                thinking,
                                unavailable,
                                options_available,
                                action_label: format!("Actions for {role_title} candidate {}", index + 1),
                                index,
                                row_count,
                                test_id: test_id.clone(),
                                engines,
                                dragged_row,
                                on_edit,
                                on_reorder,
                                on_remove,
                            }
                        }
                    }
                }
            }
        }
    }
}

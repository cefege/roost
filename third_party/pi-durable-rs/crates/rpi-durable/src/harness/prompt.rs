//! Positional system prompt planning (port of
//! `packages/durable/src/harness/prompt.ts` @ `abe508e1b`).
//!
//! A conversation's system prompt is positional: `pi.system` entries patch the
//! replayed sections and tools rather than restating the whole prompt. This
//! module replays the sections already in effect, renders the agent's sections
//! for one request, and plans the minimal `pi.system` entries that make the
//! replayed values and tool declarations equal the desired ones.
//!
//! Rust shape notes: upstream `renderSections` catches per-section throws and
//! reports them; a Rust [`PromptSection`] renderer cannot fail (it returns
//! `Option<String>`), so [`render_sections`] has no error path and drops the
//! `shown`/`report` parameters (registered FORM difference). Ordered JS `Map`s
//! become [`serde_json::Map`] (`preserve_order`), so section order is observable
//! exactly as upstream.

use std::sync::Arc;

use rpi_ai::types::{Message, SystemContent, SystemMessage, SystemRole, Tool, ToolReference};
use rpi_ai::utils::transcript::{declarations_equal, get_current_tools, to_tool_declaration};
use serde_json::{Map, Value};

use crate::chord::context::Context;
use crate::entries::SYSTEM_ENTRY;
use crate::harness::types::{ContextView, PromptInput, PromptSection};
use crate::session::EntryDraft;
use crate::types::{ContextEdit, ContextEditAction};

/// Sections in effect after replaying system messages in order: set in place,
/// `null` deletes, re-adding appends.
pub fn replay_sections(messages: &[Message]) -> Map<String, Value> {
    let mut shown = Map::new();
    for message in messages {
        let Message::System(system) = message else {
            continue;
        };
        let Some(sections) = &system.sections else {
            continue;
        };
        for (key, value) in sections {
            match value {
                Value::Null => {
                    shown.remove(key);
                }
                Value::String(text) => {
                    shown.insert(key.clone(), Value::String(text.clone()));
                }
                _ => {}
            }
        }
    }
    shown
}

/// Render the agent's sections in order. `None` omits a section; tagged text is
/// wrapped as `<key>\n...\n</key>`.
// FORM: upstream `renderSections` takes `shown` and `report` and keeps a shown
// section's text when its renderer throws; the Rust renderer cannot fail.
pub async fn render_sections(
    sections: &[PromptSection],
    input: PromptInput,
    context: &Context,
) -> Map<String, Value> {
    let mut desired = Map::new();
    for section in sections {
        let render = Arc::clone(&section.render);
        let text = (render)(input.clone(), context.clone()).await;
        let Some(text) = text else {
            continue;
        };
        let value = if section.tag {
            format!("<{}>\n{}\n</{}>", section.key, text, section.key)
        } else {
            text
        };
        desired.insert(section.key.clone(), Value::String(value));
    }
    desired
}

/// Tool changes from `offered` to `desired`.
struct ToolChanges {
    tools_removed: Vec<ToolReference>,
    tools_added: Vec<Tool>,
}

/// Plan the `pi.system` entries that make the replayed sections and tools of
/// `view` equal `desired` and `tools` in values and order.
///
/// - A head marker with no later `pi.system` entry in context: one complete
///   baseline that omits every retained earlier `pi.system` entry, written even
///   when it restates the replayed values.
/// - Otherwise, when a minimal section patch would leave a different order:
///   remove every shown section, then re-add every desired section in order.
/// - Otherwise the minimal patch of changed values and `null` removals, or
///   nothing.
///
/// Tool changes ride on the last planned entry, or on one entry of their own.
pub fn plan_system_entries(
    view: &ContextView,
    desired: &Map<String, Value>,
    tools: &[Tool],
    timestamp: i64,
) -> Vec<EntryDraft> {
    if let Some(head) = &view.head
        && !view
            .entries
            .iter()
            .any(|entry| entry.kind == SYSTEM_ENTRY && entry.id > head.id)
    {
        let edits: Vec<ContextEdit> = view
            .entries
            .iter()
            .filter(|entry| entry.kind == SYSTEM_ENTRY)
            .map(|entry| ContextEdit {
                target: entry.id,
                action: ContextEditAction::Omit,
            })
            .collect();
        let changes = ToolChanges {
            tools_removed: Vec::new(),
            tools_added: tools.iter().map(to_tool_declaration).collect(),
        };
        let mut baseline = system_entry(Some(desired.clone()), Some(&changes), timestamp);
        if !edits.is_empty() {
            baseline.edits = Some(edits);
        }
        return vec![baseline];
    }
    let sections = plan_sections(&replay_sections(&view.messages), desired);
    let changes = plan_tools(&get_current_tools(&view.messages), tools);
    if changes.tools_removed.is_empty() && changes.tools_added.is_empty() {
        return sections
            .into_iter()
            .map(|patch| system_entry(Some(patch), None, timestamp))
            .collect();
    }
    if sections.is_empty() {
        return vec![system_entry(None, Some(&changes), timestamp)];
    }
    let last = sections.len() - 1;
    sections
        .into_iter()
        .enumerate()
        .map(|(index, patch)| {
            let changes = if index == last { Some(&changes) } else { None };
            system_entry(Some(patch), changes, timestamp)
        })
        .collect()
}

/// Tool changes from `offered` to `desired`. A changed declaration is removed
/// and re-added. Replay keeps retained tools in place and appends additions;
/// when that would not yield the desired order, every offered tool is removed
/// and every desired tool re-added in order.
fn plan_tools(offered: &[Tool], desired: &[Tool]) -> ToolChanges {
    let wanted: std::collections::HashMap<&str, &Tool> = desired
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let kept: Vec<&Tool> = offered
        .iter()
        .filter(|tool| {
            wanted
                .get(tool.name.as_str())
                .is_some_and(|next| declarations_equal(tool, next))
        })
        .collect();
    let kept_names: std::collections::HashSet<&str> =
        kept.iter().map(|tool| tool.name.as_str()).collect();
    let added: Vec<&Tool> = desired
        .iter()
        .filter(|tool| !kept_names.contains(tool.name.as_str()))
        .collect();
    let replayed: Vec<&Tool> = kept.iter().copied().chain(added.iter().copied()).collect();
    let order_differs = replayed.iter().enumerate().any(|(index, tool)| {
        desired.get(index).map(|next| next.name.as_str()) != Some(tool.name.as_str())
    });
    if order_differs {
        return ToolChanges {
            tools_removed: offered
                .iter()
                .map(|tool| ToolReference {
                    name: tool.name.clone(),
                })
                .collect(),
            tools_added: desired.iter().map(to_tool_declaration).collect(),
        };
    }
    ToolChanges {
        tools_removed: offered
            .iter()
            .filter(|tool| !kept_names.contains(tool.name.as_str()))
            .map(|tool| ToolReference {
                name: tool.name.clone(),
            })
            .collect(),
        tools_added: added.iter().map(|tool| to_tool_declaration(tool)).collect(),
    }
}

/// Section patches: none, the minimal patch, or a remove-all/re-add-all pair
/// when the order would differ.
fn plan_sections(
    shown: &Map<String, Value>,
    desired: &Map<String, Value>,
) -> Vec<Map<String, Value>> {
    let mut patched_order: Vec<&String> = shown
        .keys()
        .filter(|key| desired.contains_key(*key))
        .collect();
    patched_order.extend(desired.keys().filter(|key| !shown.contains_key(*key)));
    let desired_order: Vec<&String> = desired.keys().collect();
    if patched_order
        .iter()
        .zip(desired_order.iter())
        .any(|(left, right)| left != right)
        || patched_order.len() != desired_order.len()
    {
        let removals: Map<String, Value> =
            shown.keys().map(|key| (key.clone(), Value::Null)).collect();
        return vec![removals, desired.clone()];
    }

    let mut patch = Map::new();
    for (key, value) in shown {
        let next = desired.get(key);
        if next != Some(value) {
            patch.insert(key.clone(), next.cloned().unwrap_or(Value::Null));
        }
    }
    for (key, value) in desired {
        if !shown.contains_key(key) {
            patch.insert(key.clone(), value.clone());
        }
    }
    if patch.is_empty() {
        Vec::new()
    } else {
        vec![patch]
    }
}

/// One `pi.system` entry draft carrying section and tool changes.
fn system_entry(
    sections: Option<Map<String, Value>>,
    tools: Option<&ToolChanges>,
    timestamp: i64,
) -> EntryDraft {
    let tools_removed = tools
        .filter(|changes| !changes.tools_removed.is_empty())
        .map(|changes| changes.tools_removed.clone());
    let tools_added = tools
        .filter(|changes| !changes.tools_added.is_empty())
        .map(|changes| changes.tools_added.clone());
    let message = SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections,
        tools_added,
        tools_removed,
        timestamp,
    };
    let mut draft = EntryDraft::new(SYSTEM_ENTRY);
    draft.model = Some(vec![Message::System(message)]);
    draft
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpi_ai::types::Tool;

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
        }
    }

    fn system(sections: &[(&str, Option<&str>)]) -> Message {
        let mut map = Map::new();
        for (key, value) in sections {
            map.insert(
                (*key).to_string(),
                value
                    .map(|text| Value::String(text.to_string()))
                    .unwrap_or(Value::Null),
            );
        }
        Message::System(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(String::new()),
            sections: Some(map),
            tools_added: None,
            tools_removed: None,
            timestamp: 1,
        })
    }

    #[test]
    fn replay_sets_and_deletes_sections_in_order() {
        let messages = vec![
            system(&[("a", Some("one")), ("b", Some("two"))]),
            system(&[("a", None), ("c", Some("three"))]),
        ];
        let shown = replay_sections(&messages);
        let keys: Vec<&String> = shown.keys().collect();
        assert_eq!(keys, vec!["b", "c"]);
        assert_eq!(shown.get("c"), Some(&Value::String("three".into())));
    }

    #[test]
    fn plan_sections_patches_changed_values_and_removals() {
        let mut shown = Map::new();
        shown.insert("a".to_string(), Value::String("1".into()));
        let mut desired = Map::new();
        desired.insert("a".to_string(), Value::String("2".into()));
        desired.insert("b".to_string(), Value::String("x".into()));
        let patches = plan_sections(&shown, &desired);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].get("a"), Some(&Value::String("2".into())));
        assert_eq!(patches[0].get("b"), Some(&Value::String("x".into())));
    }

    #[test]
    fn plan_tools_keeps_retained_and_appends_added() {
        let changes = plan_tools(&[tool("a")], &[tool("a"), tool("b")]);
        assert!(changes.tools_removed.is_empty());
        assert_eq!(changes.tools_added.len(), 1);
        assert_eq!(changes.tools_added[0].name, "b");
    }

    #[test]
    fn plan_tools_reorders_when_replay_order_differs() {
        let changes = plan_tools(&[tool("a"), tool("b")], &[tool("b"), tool("a")]);
        assert_eq!(changes.tools_removed.len(), 2);
        assert_eq!(changes.tools_added.len(), 2);
    }
}

//! Visible content boundaries for canonical read-only prop trees.
//! Drives the real renderer and inspects the text and controls a DOM receives.
//! No controller or generated source text is mocked or pinned.

use super::render;
use dioxus::core::{AttributeValue, Mutation, Mutations};
use dioxus::prelude::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[component]
fn CardFixture(card: Value) -> Element {
    render(&card, "surface", &BTreeMap::new())
}

fn paint(card: Value) -> Mutations {
    let mut dom = VirtualDom::new_with_props(CardFixture, CardFixtureProps { card });
    dom.rebuild_to_vec()
}

fn text(mutations: &Mutations) -> Vec<&str> {
    mutations
        .edits
        .iter()
        .filter_map(|edit| match edit {
            Mutation::CreateText { value } | Mutation::SetText { value, .. } => {
                Some(value.as_str())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn wide_and_deep_prop_trees_do_not_silently_lose_their_tail() {
    let mut deep = json!({"k":"text","p":{"text":"Deep assignment detail"}});
    for _ in 0..12 {
        deep = json!({"k":"col","c":[deep]});
    }
    let mut children: Vec<Value> = (0..320)
        .map(|index| json!({"k":"text","p":{"text":format!("Assignment row {index}")}}))
        .collect();
    children.push(deep);
    let mutations = paint(json!({"k":"col","c":children}));
    let visible = text(&mutations);
    assert!(visible.contains(&"Assignment row 319"));
    assert!(visible.contains(&"Deep assignment detail"));
}

#[test]
fn activity_cards_keep_their_details_without_native_action_targets() {
    let mutations = paint(json!({"k":"col","c":[
        {"k":"tool","p":{"name":"grep","title":"Search","status":"done","target":"needle","targetKind":"query","tools":[{"id":"rerun","label":"Repeat search"}]},"c":[{"k":"text","p":{"text":"Found assignment detail"}}]},
        {"k":"agent","p":{"name":"Reviewer","status":"done","task":"Read the complete assignment","collapsible":false},"c":[{"k":"text","p":{"text":"Nested agent evidence"}}]},
        {"k":"checklist","p":{"phases":[{"id":"review","title":"Review","items":[{"id":"safety","text":"Check safety","status":"pending"}]}]}}
    ]}));
    let visible = text(&mutations);
    for expected in [
        "Search",
        "Repeat search",
        "Found assignment detail",
        "Read the complete assignment",
        "Nested agent evidence",
        "Check safety",
    ] {
        assert!(visible.contains(&expected), "missing {expected}");
    }
}

#[test]
fn collapsed_source_previews_keep_their_head_without_revealing_hidden_body() {
    let mutations = paint(json!({"k":"col","c":[
        {"k":"section","p":{"collapsible":true,"collapsed":true},"c":[
            {"k":"text","p":{"text":"Source heading"}},
            {"k":"text","p":{"text":"Hidden section detail"}}
        ]},
        {"k":"tool","p":{"name":"read","title":"Read","collapsible":true,"collapsed":true,"preview":"none"},"c":[{"k":"text","p":{"text":"Hidden tool detail"}}]}
    ]}));
    let visible = text(&mutations);
    assert!(visible.contains(&"Source heading"));
    assert!(!visible.contains(&"Hidden section detail"));
    assert!(!visible.contains(&"Hidden tool detail"));
}

#[test]
fn unsupported_controls_are_explicit_without_echoing_private_props() {
    let mutations = paint(json!({"k":"prefs","p":{"value":"never expose private props"}}));
    let visible = text(&mutations);
    assert!(visible.iter().any(|text| text.contains("prefs")));
    assert!(!visible.iter().any(|text| text.contains("never expose")));
    assert!(
        !mutations
            .edits
            .iter()
            .any(|edit| matches!(edit, Mutation::NewEventListener { .. }))
    );
}

#[test]
fn status_segments_keep_their_motion_children_and_native_numeric_labels() {
    let mutations = paint(json!({"k":"status","c":[
        {"k":"seg","p":{"spans":[{"t":"Activity"}]},"c":[
            {"k":"rate","p":{"value":42,"unit":"tok/s"}},
            {"k":"elapsed","p":{"age":6000,"stopped":6000}}
        ]}
    ]}));
    let visible = text(&mutations);
    assert!(visible.contains(&"Activity"));
    assert!(visible.contains(&"42.0 tok/s"));
    assert!(visible.contains(&"6.0s"));
}

fn classes(mutations: &Mutations) -> Vec<&str> {
    mutations
        .edits
        .iter()
        .filter_map(|edit| match edit {
            Mutation::SetAttribute {
                name,
                value: AttributeValue::Text(value),
                ..
            } if name == "class" => Some(value.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn checklist_rich_text_retains_phase_item_and_note_styles() {
    let mutations = paint(json!({"k":"checklist","p":{
        "note":[{"t":"Reminder","s":"underline"}],
        "phases":[{"id":"review","title":[{"t":"Review","s":"bold"}],"items":[{
            "id":"safety","text":[{"t":"Check safety","s":"italic"}],
            "status":"blocked","note":[{"t":"Waiting on evidence","s":"mono"}]
        }]}]
    }}));
    let rendered_classes = classes(&mutations);
    for token in ["underline", "font-semibold", "italic", "font-mono"] {
        assert!(
            rendered_classes
                .iter()
                .any(|class| class.split_whitespace().any(|part| part == token)),
            "missing {token}"
        );
    }
}

#[test]
fn agent_task_and_running_tool_intent_keep_their_styled_spans() {
    let mutations = paint(json!({"k":"agent","p":{
        "name":"Reviewer","status":"running",
        "task":[{"t":"Review the assignment","s":"italic"}],
        "tool":{"name":"read","intent":[{"t":"Read evidence","s":"underline"}]}
    }}));
    let rendered_classes = classes(&mutations);
    for token in ["italic", "underline"] {
        assert!(
            rendered_classes
                .iter()
                .any(|class| class.split_whitespace().any(|part| part == token)),
            "missing {token}"
        );
    }
}

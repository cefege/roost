use super::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::{Value, json};

fn ui(kind: &str, props: Value, children: Vec<OmpUiNode>) -> OmpUiNode {
    OmpUiNode {
        id: "n".into(),
        k: kind.into(),
        p: props.as_object().cloned().unwrap_or_default(),
        c: children,
    }
}

#[test]
fn parses_every_native_preview_shape() {
    let parse =
        |preview: Value| Preview::declared(&ui("tool", json!({ "preview": preview }), Vec::new()));
    assert_eq!(parse(json!("none")), Some(Preview::None));
    assert_eq!(parse(json!("children")), Some(Preview::Children));
    assert_eq!(parse(json!({ "lines": 3 })), Some(Preview::Lines(3)));
    assert_eq!(parse(json!({ "tail": 4 })), Some(Preview::Tail(4)));
    assert_eq!(parse(json!("auto")), Some(Preview::Lines(10)));
    assert_eq!(parse(Value::Null), None);
}

#[test]
fn tool_defaults_follow_the_frame() {
    let card = ui("tool", json!({}), Vec::new());
    let inline = ui("tool", json!({ "frame": "inline" }), Vec::new());
    let declared = ui(
        "tool",
        json!({ "frame": "inline", "preview": { "lines": 2 } }),
        Vec::new(),
    );
    assert_eq!(Preview::for_tool(&card), Preview::Lines(10));
    assert_eq!(Preview::for_tool(&inline), Preview::None);
    assert_eq!(Preview::for_tool(&declared), Preview::Lines(2));
}

#[test]
fn card_previews_default_to_the_native_size() {
    let plain = ui("card", json!({}), Vec::new());
    let none = ui("card", json!({ "preview": "none" }), Vec::new());
    let declared = ui("card", json!({ "preview": { "lines": 4 } }), Vec::new());
    assert_eq!(Preview::for_card(&plain), Preview::Lines(10));
    assert_eq!(Preview::for_card(&none), Preview::None);
    assert_eq!(Preview::for_card(&declared), Preview::Lines(4));
}

#[test]
fn a_fitting_leaf_stays_a_full_reference() {
    let children = vec![ui("code", json!({ "text": "one\ntwo\nthree" }), Vec::new())];
    let mut budget = 5;
    let selected = select_children(&children, &mut budget, false);
    assert_eq!(selected.len(), 1);
    assert!(matches!(selected.first(), Some(WindowNode::Full(_))));
    assert_eq!(budget, 2);
    assert_eq!(
        children[0].p.get("text").and_then(Value::as_str),
        Some("one\ntwo\nthree")
    );
}

#[test]
fn head_and_tail_select_line_ranges_without_touching_source() {
    let source = "one\ntwo\nthree\nfour";
    let children = vec![ui("code", json!({ "text": source }), Vec::new())];

    let mut head_budget = 2;
    let head = select_children(&children, &mut head_budget, false);
    match head.first() {
        Some(WindowNode::Text { node, range }) => {
            assert_eq!(range, &(0..2));
            assert_eq!(node.p.get("text").and_then(Value::as_str), Some(source));
        }
        _ => panic!("expected a head text window"),
    }

    let mut tail_budget = 2;
    let tail = select_children(&children, &mut tail_budget, true);
    match tail.first() {
        Some(WindowNode::Text { node, range }) => {
            assert_eq!(range, &(2..4));
            assert_eq!(node.p.get("text").and_then(Value::as_str), Some(source));
        }
        _ => panic!("expected a tail text window"),
    }
}

#[test]
fn ansi_tail_keeps_full_source_and_preview_for_copy_and_sgr() {
    let source = "\u{1b}[31mred\u{1b}[0m\nplain\ntail";
    let children = vec![ui(
        "ansi",
        json!({ "text": source, "follow": true, "preview": { "lines": 2 } }),
        Vec::new(),
    )];
    let mut budget = 2;
    let selected = select_children(&children, &mut budget, true);
    match selected.first() {
        Some(WindowNode::Text { node, range }) => {
            assert_eq!(range, &(1..3));
            assert_eq!(node.p.get("text").and_then(Value::as_str), Some(source));
            assert!(
                node.p.get("preview").is_some(),
                "RichContent parses the untouched source and owns the window"
            );
        }
        _ => panic!("expected an ANSI tail window"),
    }
}

#[test]
fn a_window_spans_children_in_source_order() {
    let children = vec![
        ui("code", json!({ "text": "one\ntwo" }), Vec::new()),
        ui("code", json!({ "text": "three\nfour" }), Vec::new()),
    ];
    let mut budget = 3;
    let selected = select_children(&children, &mut budget, false);
    assert_eq!(selected.len(), 2);
    assert!(matches!(selected.first(), Some(WindowNode::Full(_))));
    match selected.get(1) {
        Some(WindowNode::Text { range, .. }) => assert_eq!(range, &(0..1)),
        _ => panic!("expected the second child to be cut"),
    }
}

#[test]
fn a_structural_box_keeps_only_its_selected_children() {
    let column = ui(
        "col",
        json!({}),
        vec![
            ui("code", json!({ "text": "one\ntwo" }), Vec::new()),
            ui("code", json!({ "text": "three\nfour" }), Vec::new()),
        ],
    );
    let children = vec![column];
    let mut budget = 3;
    let selected = select_children(&children, &mut budget, false);
    match selected.first() {
        Some(WindowNode::Structure { node, children }) => {
            assert_eq!(node.k, "col");
            assert_eq!(children.len(), 2);
            match children.get(1) {
                Some(WindowNode::Text { range, .. }) => assert_eq!(range, &(0..1)),
                _ => panic!("expected the second body child to be cut"),
            }
        }
        _ => panic!("expected a structural window"),
    }
}

#[test]
fn a_row_is_one_drawn_line() {
    let rows = [ui(
        "row",
        json!({}),
        vec![
            ui("text", json!({ "text": "state" }), Vec::new()),
            ui("text", json!({ "text": "facts" }), Vec::new()),
        ],
    )];
    let mut budget = 1;
    let selected = select_children(&rows, &mut budget, false);
    assert_eq!(selected.len(), 1);
    assert!(matches!(selected.first(), Some(WindowNode::Full(_))));
    assert_eq!(budget, 0);
}

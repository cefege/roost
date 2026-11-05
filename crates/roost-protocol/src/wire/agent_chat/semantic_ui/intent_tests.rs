//! Surface-local transient reveal and scroll intent isolation.
//! Exercises document adoption, bounded pending state and snapshot persistence.
//! Uses the shared frame fixtures from the parent regression module.

use super::*;
fn intent_frame(sequence: u64, ops: serde_json::Value) -> serde_json::Value {
    json!({"verb":"f","body":{"sf":"s:0","s":sequence,"ops":ops}})
}

#[test]
fn reveal_and_scroll_requests_wait_for_the_paint_then_leave() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document
        .apply_message(&intent_frame(
            2,
            json!([
                ["reveal", "entry", "nearest"],
                ["scroll", "entry", "line-down"],
                ["scroll", "entry", "end"]
            ]),
        ))
        .unwrap();
    let expected = [
        OmpUiIntent::Reveal {
            id: "entry".into(),
            at: OmpUiRevealAt::Nearest,
        },
        OmpUiIntent::Scroll {
            id: "entry".into(),
            by: OmpUiScrollBy::LineDown,
            repeat: 1,
        },
        OmpUiIntent::Scroll {
            id: "entry".into(),
            by: OmpUiScrollBy::End,
            repeat: 1,
        },
    ];
    assert_eq!(document.surfaces["s:0"].intents(), expected.as_slice());
    assert_eq!(document.take_intents("s:0"), expected.to_vec());
    assert!(document.surfaces["s:0"].intents().is_empty());
    assert!(document.take_intents("s:0").is_empty());
    assert!(document.take_intents("absent").is_empty());
}

#[test]
fn invalid_placement_direction_or_reference_rejects_the_whole_frame() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    let accepted = document.surfaces["s:0"].clone();
    for ops in [
        json!([["reveal", "entry", "middle"]]),
        json!([["scroll", "entry", "sideways"]]),
        json!([["scroll", "missing", "line-down"]]),
        json!([["reveal", "entry", "nearest"], ["del", "missing"]]),
    ] {
        assert!(document.apply_message(&intent_frame(2, ops)).is_err());
        assert_eq!(document.surfaces["s:0"], accepted);
        assert!(document.surfaces["s:0"].intents().is_empty());
    }
    document
        .apply_message(&intent_frame(2, json!([["reveal", "entry", "nearest"]])))
        .unwrap();
    assert_eq!(document.surfaces["s:0"].intents().len(), 1);
    assert!(
        document
            .apply_message(&intent_frame(
                3,
                json!([
                    ["scroll", "entry", "line-up"],
                    ["reveal", "entry", "middle"]
                ])
            ))
            .is_err()
    );
    assert_eq!(document.surfaces["s:0"].intents().len(), 1);
    document
        .apply_message(&intent_frame(3, json!([["settle", "entry"]])))
        .unwrap();
    assert_eq!(document.surfaces["s:0"].intents().len(), 1);
}

#[test]
fn frames_coalesced_before_a_paint_keep_every_press_once() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    for sequence in 2..=3 {
        document
            .apply_message(&intent_frame(
                sequence,
                json!([["reveal", "entry", "nearest"]]),
            ))
            .unwrap();
    }
    for sequence in 4..=5 {
        document
            .apply_message(&intent_frame(
                sequence,
                json!([["scroll", "entry", "line-down"]]),
            ))
            .unwrap();
    }
    document
        .apply_message(&intent_frame(6, json!([["scroll", "entry", "page-up"]])))
        .unwrap();
    let expected = [
        OmpUiIntent::Reveal {
            id: "entry".into(),
            at: OmpUiRevealAt::Nearest,
        },
        OmpUiIntent::Scroll {
            id: "entry".into(),
            by: OmpUiScrollBy::LineDown,
            repeat: 2,
        },
        OmpUiIntent::Scroll {
            id: "entry".into(),
            by: OmpUiScrollBy::PageUp,
            repeat: 1,
        },
    ];
    assert_eq!(document.surfaces["s:0"].intents(), expected.as_slice());
}

#[test]
fn closing_or_adopting_a_surface_drops_its_pending_requests() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document
        .apply_message(&intent_frame(2, json!([["reveal", "entry", "start"]])))
        .unwrap();
    assert!(!document.surfaces["s:0"].intents().is_empty());
    document
        .apply_message(&json!({"verb":"x","body":{"id":"s:0","keep":true}}))
        .unwrap();
    assert!(document.surfaces["s:0"].intents().is_empty());
    document
        .apply_message(&json!({"verb":"o","body":{"id":"s:0","adopt":true}}))
        .unwrap();
    assert!(document.surfaces["s:0"].intents().is_empty());
    document
        .apply_message(&intent_frame(3, json!([["reveal", "s:0", "end"]])))
        .unwrap();
    assert!(!document.surfaces["s:0"].intents().is_empty());
    document
        .apply_message(&json!({"verb":"x","body":{"id":"s:0","keep":false}}))
        .unwrap();
    open(&mut document);
    assert!(document.surfaces["s:0"].intents().is_empty());
}

#[test]
fn requests_never_outlive_the_node_they_address() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document
        .apply_message(&intent_frame(
            2,
            json!([
                ["reveal", "entry", "nearest"],
                ["scroll", "entry", "line-down"],
                ["del", "entry"]
            ]),
        ))
        .unwrap();
    assert!(document.surfaces["s:0"].intents().is_empty());
    document
        .apply_message(&intent_frame(
            3,
            json!([["add","again","main",null,{"id":"again","k":"text"}]]),
        ))
        .unwrap();
    document
        .apply_message(&intent_frame(4, json!([["scroll", "again", "end"]])))
        .unwrap();
    assert_eq!(document.surfaces["s:0"].intents().len(), 1);
    document
        .apply_message(&intent_frame(5, json!([["del", "again"]])))
        .unwrap();
    assert!(document.surfaces["s:0"].intents().is_empty());
}

#[test]
fn surfaces_and_documents_keep_their_requests_apart() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document
        .apply_message(&json!({"verb":"o","body":{"id":"s:1","mode":"screen"}}))
        .unwrap();
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:1","s":1,"ops":[
            ["add","body","s:1",null,{"id":"body","k":"col"}]
        ]}}))
        .unwrap();
    document
        .apply_message(&intent_frame(2, json!([["reveal", "entry", "nearest"]])))
        .unwrap();
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:1","s":2,"ops":[
            ["scroll","body","page-down"]
        ]}}))
        .unwrap();
    assert_eq!(document.take_intents("s:0").len(), 1);
    assert_eq!(document.surfaces["s:1"].intents().len(), 1);
    document
        .apply_message(&json!({"verb":"o","body":{"id":"s:1","adopt":true}}))
        .unwrap();
    assert!(document.surfaces["s:1"].intents().is_empty());
}

#[test]
fn pending_requests_are_bounded_and_never_serialized() {
    use super::operations::record_intent;
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document
        .apply_message(&intent_frame(2, json!([["reveal", "entry", "nearest"]])))
        .unwrap();
    let serialized = serde_json::to_value(&document).unwrap();
    assert!(serialized["surfaces"]["s:0"].get("intents").is_none());
    let restored: OmpUiDocument = serde_json::from_value(serialized).unwrap();
    assert!(restored.surfaces["s:0"].intents().is_empty());
    let mut surface = document.surfaces.remove("s:0").unwrap();
    for index in 0..=(MAX_TSP_INTENTS) {
        let id = if index % 2 == 0 { "entry" } else { "dock" };
        record_intent(
            &mut surface,
            OmpUiIntent::Scroll {
                id: id.into(),
                by: OmpUiScrollBy::LineDown,
                repeat: 1,
            },
        );
    }
    assert_eq!(surface.intents().len(), MAX_TSP_INTENTS);
    assert!(
        surface
            .intents()
            .iter()
            .all(|intent| matches!(intent, OmpUiIntent::Scroll { .. }))
    );
}

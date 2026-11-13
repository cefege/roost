use super::*;
use serde_json::json;

fn open(document: &mut OmpUiDocument) {
    document
        .apply_message(&json!({"verb":"o","body":{"id":"s:0","mode":"inline"}}))
        .unwrap();
}

fn first_native_frame(document: &mut OmpUiDocument) {
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":1,"ops":[
            ["add","main","s:0",null,{"id":"main","k":"col"}],
            ["add","dock","s:0",null,{"id":"dock","k":"col"}],
            ["add","layer","s:0",null,{"id":"layer","k":"col"}],
            ["add","entry","main",null,{"id":"entry","k":"text","p":{"text":"hello"}}]
        ]}}))
        .unwrap();
}

#[test]
fn native_regions_and_partial_frame_rollback() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    let accepted = document.surfaces["s:0"].clone();
    assert!(
        document
            .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":2,"ops":[
                ["set","entry",{"text":"changed"}], ["del","missing"]
            ]}}))
            .is_err()
    );
    assert_eq!(document.surfaces["s:0"], accepted);
}

#[test]
fn splice_uses_utf16_offsets_and_adoption_keeps_sequence() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":1,"ops":[
            ["add","main","s:0",null,{"id":"main","k":"col"}],
            ["add","dock","s:0",null,{"id":"dock","k":"col"}],
            ["add","layer","s:0",null,{"id":"layer","k":"col"}],
            ["add","entry","main",null,{"id":"entry","k":"text","p":{"text":"😀ab"}}]
        ]}}))
        .unwrap();
    document
        .apply_message(&json!({"verb":"o","body":{"id":"s:0","mode":"inline","adopt":true}}))
        .unwrap();
    document
        .apply_message(
            &json!({"verb":"f","body":{"sf":"s:0","s":2,"ops":[["splice","entry",2,1,"x"]]}}),
        )
        .unwrap();
    assert_eq!(document.surfaces["s:0"].root.c[0].c[0].p["text"], "😀xb");
    let mut missing = OmpUiDocument::default();
    assert!(matches!(
        missing.apply_message(&json!({"verb":"o","body":{"id":"gone","adopt":true}})),
        Err(OmpUiDocumentError::Gone)
    ));
}

#[test]
fn rejects_duplicate_ids_and_bad_sequences_without_mutation() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    assert!(
        document
            .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":1,"ops":[
                ["add","main","s:0",null,{"id":"main","k":"col"}],
                ["add","dock","s:0",null,{"id":"dock","k":"col"}],
                ["add","layer","s:0",null,{"id":"layer","k":"col"}],
                ["add","first","main",null,{"id":"first","k":"text"}],
                ["add","first","dock",null,{"id":"first","k":"text"}]
            ]}}))
            .is_err()
    );
    assert_eq!(document.surfaces["s:0"].sequence, 0);
    assert!(document.surfaces["s:0"].root.c.is_empty());
}

#[test]
fn oversized_autocomplete_id_rejects_frame_and_fences_later_frames() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":2,"ops":[
            ["text","entry","append","/resume /long/path"]
        ]}}))
        .unwrap();
    let accepted = document.surfaces["s:0"].clone();
    let oversized_id = format!("list/{}", "a".repeat(377));
    assert_eq!(
        document.apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":3,"ops":[
            ["add","popup","layer",null,{"id":"popup","k":"overlay","c":[
                {"id":oversized_id,"k":"item"}
            ]}]
        ]}})),
        Err(OmpUiDocumentError::Invalid("id"))
    );
    assert_eq!(
        document.apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":4,"ops":[]}})),
        Err(OmpUiDocumentError::Sequence)
    );
    assert_eq!(document.surfaces["s:0"], accepted);
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":3,"ops":[
            ["add","popup","layer",null,{"id":"popup","k":"overlay","c":[
                {"id":"list/opaque-1","k":"item"}
            ]}]
        ]}}))
        .unwrap();
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":4,"ops":[
            ["del","popup"]
        ]}}))
        .unwrap();
}

#[test]
fn accepts_native_blob_body_and_headers() {
    let mut document = OmpUiDocument::default();
    document
        .apply_message(
            &json!({"verb":"b","headers":{"id":"sha256-1","mime":"image/png"},"body":"aGVsbG8="}),
        )
        .unwrap();
    assert_eq!(document.blobs["sha256-1"].mime, "image/png");
}

#[test]
fn query_requires_native_payload_shape() {
    let mut document = OmpUiDocument::default();
    assert!(
        document
            .apply_message(&json!({"verb":"q","body":{"q":"hello"}}))
            .is_err()
    );
    assert!(
        document
            .apply_message(&json!({"verb":"q","body":{"q":"hello","v":[1],"app":"omp"}}))
            .is_ok()
    );
}

#[test]
fn failed_mixed_frame_restores_nodes_catalogue_text_and_runtime_indexes() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    document.apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":2,"ops":[
        ["add","picker","dock",null,{"id":"picker","k":"picker","p":{"items":[{"id":"old","label":"Original"}]}}]
    ]}})).unwrap();
    let accepted = document.surfaces["s:0"].clone();
    let rejected = json!({"verb":"f","body":{"sf":"s:0","s":3,"ops":[
        ["text","entry","append"," appended"],
        ["splice","entry",0,1,"H"],
        ["set","picker",{"itemsDel":["old"],"itemsAdd":[{"id":"new","label":"New"}]}],
        ["move","entry","dock","picker"],
        ["del","picker"],
        ["add","temporary","main",null,{"id":"temporary","k":"text"}],
        ["del","missing"]
    ]}});
    assert!(document.apply_message(&rejected).is_err());
    assert_eq!(document.surfaces["s:0"], accepted);
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":3,"ops":[
            ["text","entry","append","!"],
            ["set","picker",{"itemsAdd":[{"id":"new","label":"New"}]}]
        ]}}))
        .unwrap();
    assert_eq!(document.surfaces["s:0"].root.c[0].c[0].p["text"], "hello!");
    assert_eq!(
        document.surfaces["s:0"].root.c[1].c[0].p["items"],
        json!([{"id":"old","label":"Original"},{"id":"new","label":"New"}])
    );
}

#[test]
fn recovered_snapshot_rebuilds_indexes_and_adopted_live_regions() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    let mut restored: OmpUiDocument =
        serde_json::from_value(serde_json::to_value(&document).unwrap()).unwrap();
    restored
        .apply_message(&json!({"verb":"x","body":{"id":"s:0","keep":true}}))
        .unwrap();
    restored
        .apply_message(&json!({"verb":"o","body":{"id":"s:0","adopt":true}}))
        .unwrap();
    restored
        .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":2,"ops":[
            ["add","dock","s:0",null,{"id":"dock","k":"col"}],
            ["add","layer","s:0",null,{"id":"layer","k":"col"}],
            ["text","entry","append"," restored"]
        ]}}))
        .unwrap();
    assert_eq!(
        restored.surfaces["s:0"].root.c[0].c[0].p["text"],
        "hello restored"
    );
    assert!(!restored.surfaces["s:0"].suspended);
}

#[test]
fn aggregate_stream_limit_rejects_growth_atomically() {
    let mut document = OmpUiDocument::default();
    open(&mut document);
    first_native_frame(&mut document);
    let chunk = "x".repeat(1024 * 1024);
    for sequence in 2..=32 {
        document
            .apply_message(&json!({"verb":"f","body":{"sf":"s:0","s":sequence,"ops":[
                ["text","entry","append",chunk]
            ]}}))
            .unwrap();
    }
    let rejected =
        json!({"verb":"f","body":{"sf":"s:0","s":33,"ops":[["text","entry","append",chunk]]}});
    assert_eq!(
        document.apply_message(&rejected),
        Err(OmpUiDocumentError::Limit)
    );
    assert_eq!(document.surfaces["s:0"].sequence, 32);
    assert_eq!(
        document.surfaces["s:0"].root.c[0].c[0].p["text"]
            .as_str()
            .unwrap()
            .len(),
        5 + 31 * 1024 * 1024
    );
    document
        .apply_message(
            &json!({"verb":"f","body":{"sf":"s:0","s":33,"ops":[["text","entry","append","!"]]}}),
        )
        .unwrap();
    assert!(
        document.surfaces["s:0"].root.c[0].c[0].p["text"]
            .as_str()
            .unwrap()
            .ends_with('!')
    );
}

#[path = "intent_tests.rs"]
mod intents;

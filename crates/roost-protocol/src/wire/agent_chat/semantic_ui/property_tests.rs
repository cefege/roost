//! Native property removal and picker delta lifetime regressions.
//! Applies real frames through the shared document fold; catalogue rows survive
//! clearing a transient patch and rejected frames restore every touched value.

use super::*;
use serde_json::json;

fn catalogue() -> OmpUiDocument {
    let mut document = OmpUiDocument::default();
    document
        .apply_message(&json!({"verb":"o","body":{"id":"s","mode":"inline"}}))
        .unwrap();
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s","s":1,"ops":[
            ["add","picker","s",null,{"id":"picker","k":"picker","p":{
                "subtitle":"Old filter","query":"","items":[{"id":"a","label":"Original"}]
            }}]
        ]}}))
        .unwrap();
    document
}

#[test]
fn clearing_catalogue_deltas_keeps_upserts_and_removes_stale_properties() {
    let mut document = catalogue();
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s","s":2,"ops":[
            ["set","picker",{"itemsAdd":[{"id":"a","label":"Updated"},{"id":"b","label":"Added"}]}]
        ]}}))
        .unwrap();
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s","s":3,"ops":[
            ["set","picker",{"itemsAdd":null,"itemsDel":null,"subtitle":null,"query":null}]
        ]}}))
        .unwrap();
    let props = &document.surfaces["s"].root.c[0].p;
    assert_eq!(
        props["items"],
        json!([{"id":"a","label":"Updated"},{"id":"b","label":"Added"}])
    );
    for removed in ["itemsAdd", "itemsDel", "subtitle", "query"] {
        assert!(!props.contains_key(removed), "{removed} was not removed");
    }
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s","s":4,"ops":[
            ["set","picker",{"itemsDel":["a"]}]
        ]}}))
        .unwrap();
    assert_eq!(
        document.surfaces["s"].root.c[0].p["items"],
        json!([{"id":"b","label":"Added"}])
    );
}

#[test]
fn an_invalid_catalogue_patch_rolls_back_property_removal_and_upserts() {
    let mut document = catalogue();
    let accepted = document.surfaces["s"].clone();
    assert!(
        document
            .apply_message(&json!({"verb":"f","body":{"sf":"s","s":2,"ops":[
                ["set","picker",{"subtitle":null,"itemsAdd":[{"id":"a","label":"Changed"},false]}]
            ]}}))
            .is_err()
    );
    assert_eq!(document.surfaces["s"], accepted);
    document
        .apply_message(&json!({"verb":"f","body":{"sf":"s","s":2,"ops":[
            ["set","picker",{"subtitle":null,"itemsAdd":null}]
        ]}}))
        .unwrap();
    assert_eq!(
        document.surfaces["s"].root.c[0].p["items"],
        json!([{"id":"a","label":"Original"}])
    );
}

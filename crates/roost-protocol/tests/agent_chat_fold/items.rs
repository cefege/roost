//! Shared fold coverage for retained plan, notice and advisory items.
//! Uses the parent transcript fixture and canonical fold, never a UI projection.
//! Native history remains readable while its execution surface is archived.

use super::*;
#[test]
fn notice_plan_and_advisory_items_fold_and_plan_updates_replace() {
    let mut value = transcript();
    for item in [
        TranscriptItem::Notice {
            id: "n".into(),
            level: "warn".into(),
            title: "Warning".into(),
            body: "Details".into(),
        },
        TranscriptItem::Plan {
            id: "p".into(),
            title: "Plan".into(),
            content: "Steps".into(),
            state: "proposed".into(),
        },
        TranscriptItem::Advisory {
            id: "adv".into(),
            severity: "concern".into(),
            note: "Check this".into(),
            delivered: false,
        },
        TranscriptItem::Plan {
            id: "p".into(),
            title: "Plan".into(),
            content: "Updated".into(),
            state: "approved".into(),
        },
    ] {
        fold_chat_event(&mut value, &ChatEvent::Item { item });
    }
    assert!(matches!(&value.items[2], TranscriptItem::Notice { body, .. } if body == "Details"));
    assert!(
        matches!(&value.items[3], TranscriptItem::Plan { state, content, .. } if state == "approved" && content == "Updated")
    );
    assert!(matches!(
        &value.items[4],
        TranscriptItem::Advisory {
            delivered: false,
            ..
        }
    ));
}

#[test]
fn tool_children_default_when_omitted_from_json() {
    let item: TranscriptItem = serde_json::from_str(
        r#"{"kind":"tool","id":"t","call_id":"c","tool_name":"task","args_json":"{}","output":"","is_error":false,"running":false}"#,
    ).expect("legacy tool item");
    assert!(matches!(item, TranscriptItem::Tool { children, .. } if children.is_empty()));
}

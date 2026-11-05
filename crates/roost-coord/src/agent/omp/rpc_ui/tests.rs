//! Admission bounds and origin-stripping for semantic UI actions.

use super::*;

#[test]
fn actions_are_typed_bounded_and_cannot_choose_identity_or_directory() {
    assert!(parse_action(r#"{"method":"input","params":{"data":"\u001b[A"}}"#).is_ok());
    assert!(parse_action(
            r#"{"method":"input","params":{"data":"\u001b[A","before":{"ev":"edit","from":2,"to":2,"text":"","len":5,"cursor":2}}}"#
        ).is_ok());
    assert!(
        parse_action(
            r#"{"method":"input","params":{"data":"\u001b[A","before":{"ev":"ack","id":"x"}}}"#
        )
        .is_err()
    );
    for action in [
        r#"{"method":"event","params":{"event":{"sessionId":"other"}}}"#,
        r#"{"method":"event","params":{"event":{"cwd":"/tmp"}}}"#,
        r#"{"method":"open","params":{"cols":80,"rows":24,"dark":true,"reduceMotion":false,"cwd":"/tmp"}}"#,
        r#"{"method":"close","params":{"sessionId":"other"}}"#,
        r#"{"method":"unknown","params":{}}"#,
        r#"{"method":"command","params":{"text":"x","extra":true}}"#,
    ] {
        assert!(parse_action(action).is_err(), "accepted {action}");
    }
    let large = "x".repeat(MAX_INPUT_BYTES + 1);
    let action = serde_json::json!({"method":"input","params":{"data":large}});
    assert!(parse_action(&action.to_string()).is_err());
}

#[test]
fn open_dimensions_and_nested_event_depth_are_bounded() {
    assert!(
            parse_action(
                r#"{"method":"open","params":{"cols":80,"rows":24,"dark":true,"reduceMotion":false,"kinds":["input"],"features":[]}}"#
            )
            .is_ok()
        );
    assert!(
            parse_action(
                r#"{"method":"open","params":{"cols":80,"rows":24,"dark":true,"reduceMotion":false,"reset":true}}"#
            )
            .is_ok()
        );
    assert!(
            parse_action(
                r#"{"method":"open","params":{"cols":80,"rows":24,"dark":true,"reduceMotion":false,"reset":"true"}}"#
            )
            .is_err()
        );
    assert!(
        parse_action(
            r#"{"method":"open","params":{"cols":0,"rows":24,"dark":true,"reduceMotion":false}}"#
        )
        .is_err()
    );
    let mut event = serde_json::json!({});
    for _ in 0..34 {
        event = serde_json::json!([event]);
    }
    let action = serde_json::json!({"method":"event","params":{"event":event}});
    assert!(parse_action(&action.to_string()).is_err());
}

#[test]
fn host_replies_accept_only_the_service_answer_and_never_origin_identity() {
    assert!(
        parse_action(r#"{"method":"host_reply","params":{"serviceId":"svc-1","accepted":true}}"#)
            .is_ok()
    );
    assert!(
            parse_action(
                r#"{"method":"host_reply","params":{"serviceId":"svc-1","accepted":false,"error":"denied"}}"#
            )
            .is_ok()
        );
    for action in [
        r#"{"method":"host_reply","params":{"serviceId":"svc-1","accepted":true,"viewId":"other"}}"#,
        r#"{"method":"host_reply","params":{"serviceId":"","accepted":true}}"#,
        r#"{"method":"host_reply","params":{"serviceId":"svc-1"}}"#,
        r#"{"method":"host_reply","params":{"serviceId":"svc-1","accepted":true,"clientId":"other"}}"#,
    ] {
        assert!(parse_action(action).is_err(), "accepted {action}");
    }
    let large = serde_json::json!({
        "method": "host_reply",
        "params": {"serviceId": "svc-1", "accepted": false, "error": "e".repeat(4097)},
    });
    assert!(parse_action(&large.to_string()).is_err());
}

#[test]
fn a_single_bracketed_paste_up_to_the_native_input_limit_is_admitted() {
    assert_eq!(
        MAX_ACTION_BYTES,
        roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_MESSAGE_BYTES
    );
    let at_limit = "x".repeat(MAX_INPUT_BYTES);
    let action = serde_json::json!({"method":"input","params":{"data":at_limit}});
    assert!(parse_action(&action.to_string()).is_ok());
    let over_limit = "x".repeat(MAX_INPUT_BYTES + 1);
    let action = serde_json::json!({"method":"input","params":{"data":over_limit}});
    assert!(parse_action(&action.to_string()).is_err());
}

#[test]
fn an_action_past_the_native_message_limit_is_refused_before_parsing() {
    let oversized = "x".repeat(MAX_ACTION_BYTES + 1);
    assert!(parse_action(&oversized).is_err());
}

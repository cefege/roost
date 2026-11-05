//! Unit tests for the host-service controller.
//! They cover the bounded attempt state, the copy disclosure wording, and the
//! link resolver the prompt shares with the rest of the semantic UI.

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::client::agent_chat::{OmpHostService, OmpHostServiceKind};

use super::super::links::{HostOpenTarget, resolve_host_open};
use super::controller::prune_attempt_state;

fn ids(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn a_copy_prompt_never_renders_its_value_into_debug_output() {
    let service = OmpHostService {
        service_id: "svc".into(),
        origin_request_id: "req".into(),
        conversation_id: "conv".into(),
        view_id: "view".into(),
        transport_id: "transport".into(),
        kind: OmpHostServiceKind::CopyText,
        value: "super-secret".into(),
        cancelled: false,
    };
    let debug = format!("{service:?}");
    assert!(!debug.contains("super-secret"), "{debug}");
    assert!(debug.contains("value_bytes"));
}

#[test]
fn attempt_state_drops_every_service_the_inbox_no_longer_holds() {
    let active = ids(&["kept"]);
    let mut attempted = ids(&["kept", "completed"]);
    let mut sending = ids(&["kept", "completed"]);
    let mut failures = BTreeMap::from([
        ("kept".to_owned(), "shown".to_owned()),
        ("completed".to_owned(), "stale".to_owned()),
    ]);
    prune_attempt_state(&active, &mut attempted, &mut sending, &mut failures);
    assert_eq!(attempted, ids(&["kept"]));
    assert_eq!(sending, ids(&["kept"]));
    assert_eq!(
        failures,
        BTreeMap::from([("kept".to_owned(), "shown".to_owned())])
    );
}

#[test]
fn a_file_target_navigates_and_an_external_target_stays_external() {
    let worker = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_eq!(
        resolve_host_open("https://example.com/auth", worker, "/work", Some("linux")),
        Ok(HostOpenTarget::Browser("https://example.com/auth".into()))
    );
    assert!(matches!(
        resolve_host_open("/work/out.txt", worker, "/work", Some("linux")),
        Ok(HostOpenTarget::WorkerFile(_))
    ));
    assert!(resolve_host_open("javascript:alert(1)", worker, "/work", Some("linux")).is_err());
}

//! Behaviour of the account-scoped client-state RPCs: the allowlist and caps,
//! all-or-nothing batches, and last-writer-wins on `updated_ms`.

mod db_support;
mod workspaces_support;

use connectrpc::ErrorCode;
use roost_coord::auth::principal::Principal;
use roost_coord::client_state::{handle_client_state_list, handle_client_state_set};
use roost_coord::coord_core::{Caller, ListenerTrust};
use roost_proto::{ClientStateEntry, ClientStateListRequest, ClientStateSetRequest};
use roost_protocol::client_state::{
    MAX_BATCH_ENTRIES, MAX_KEY_BYTES, MAX_KEYS_PER_ACCOUNT, MAX_VALUE_BYTES,
};
use workspaces_support::{WorkspacesFixture, machine_caller};

const DEVICE_FP: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const DRAFT_PREFIX: &str = "roost.agent.draft.";
const PANE_LAYOUT: &str = "roost.paneLayout.v1";

/// A device of the fixture's own self-hosted account: the client-state rows
/// reference that account, and the workspaces helper's account does not exist.
fn device_caller(fixture: &WorkspacesFixture) -> Caller {
    let account_id = fixture
        .core
        .services
        .boot
        .require_tenant()
        .expect("the self-hosted tenant")
        .account_id
        .clone();
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: DEVICE_FP.to_owned(),
            label: "test device".to_owned(),
            account_id,
        },
        tab_id: None,
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

fn entry(key: &str, value: &str, updated_ms: i64) -> ClientStateEntry {
    ClientStateEntry {
        key: key.to_owned(),
        value: value.to_owned(),
        updated_ms,
        ..Default::default()
    }
}

async fn set(
    fixture: &WorkspacesFixture,
    entries: Vec<ClientStateEntry>,
) -> Result<Vec<ClientStateEntry>, ErrorCode> {
    handle_client_state_set(
        &fixture.core,
        &device_caller(fixture),
        ClientStateSetRequest {
            entries,
            ..Default::default()
        },
    )
    .await
    .map(|response| response.body.entries)
    .map_err(|error| error.code)
}

async fn list(fixture: &WorkspacesFixture) -> Vec<ClientStateEntry> {
    handle_client_state_list(
        &fixture.core,
        &device_caller(fixture),
        ClientStateListRequest::default(),
    )
    .await
    .expect("the list")
    .body
    .entries
}

#[tokio::test]
async fn an_account_with_no_client_state_lists_nothing() {
    let fixture = WorkspacesFixture::new("client-state-empty").await;
    assert!(list(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_set_is_listed_back_with_its_stamp() {
    let fixture = WorkspacesFixture::new("client-state-round-trip").await;
    set(&fixture, vec![entry(PANE_LAYOUT, "{\"panes\":2}", 500)])
        .await
        .expect("stored");

    let listed = list(&fixture).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, PANE_LAYOUT);
    assert_eq!(listed[0].value, "{\"panes\":2}");
    assert_eq!(listed[0].updated_ms, 500);
}

#[tokio::test]
async fn an_older_write_loses_and_the_response_returns_the_winner() {
    let fixture = WorkspacesFixture::new("client-state-older").await;
    set(&fixture, vec![entry(PANE_LAYOUT, "newer", 200)])
        .await
        .expect("stored");

    let response = set(&fixture, vec![entry(PANE_LAYOUT, "older", 100)])
        .await
        .expect("answered");
    assert_eq!(response.len(), 1);
    assert_eq!(response[0].value, "newer");
    assert_eq!(response[0].updated_ms, 200);
    assert_eq!(list(&fixture).await[0].value, "newer");
}

#[tokio::test]
async fn an_equal_stamp_takes_the_incoming_write() {
    let fixture = WorkspacesFixture::new("client-state-tie").await;
    set(&fixture, vec![entry(PANE_LAYOUT, "first", 300)])
        .await
        .expect("stored");

    let response = set(&fixture, vec![entry(PANE_LAYOUT, "second", 300)])
        .await
        .expect("answered");
    assert_eq!(response[0].value, "second");
    assert_eq!(list(&fixture).await[0].value, "second");
}

#[tokio::test]
async fn a_batch_over_the_entry_cap_is_rejected_whole() {
    let fixture = WorkspacesFixture::new("client-state-batch-cap").await;
    let entries: Vec<ClientStateEntry> = (0..=MAX_BATCH_ENTRIES)
        .map(|index| entry(&format!("{DRAFT_PREFIX}b{index}"), "x", 1))
        .collect();
    assert_eq!(entries.len(), MAX_BATCH_ENTRIES + 1);

    assert_eq!(
        set(&fixture, entries).await.err(),
        Some(ErrorCode::InvalidArgument)
    );
    assert!(list(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_key_outside_the_allowlist_rejects_the_whole_batch() {
    let fixture = WorkspacesFixture::new("client-state-allowlist").await;
    let refused = set(
        &fixture,
        vec![
            entry(PANE_LAYOUT, "ok", 1),
            entry("roost.notSynced", "x", 1),
        ],
    )
    .await;
    assert_eq!(refused.err(), Some(ErrorCode::InvalidArgument));
    assert!(list(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_key_over_256_bytes_is_rejected() {
    let fixture = WorkspacesFixture::new("client-state-key-length").await;
    let long_key = format!("{DRAFT_PREFIX}{}", "k".repeat(MAX_KEY_BYTES));
    assert!(long_key.len() > MAX_KEY_BYTES);

    assert_eq!(
        set(&fixture, vec![entry(&long_key, "x", 1)]).await.err(),
        Some(ErrorCode::InvalidArgument)
    );
    assert!(list(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_value_over_256_kib_is_rejected() {
    let fixture = WorkspacesFixture::new("client-state-value-length").await;
    let oversized = "v".repeat(MAX_VALUE_BYTES + 1);

    assert_eq!(
        set(&fixture, vec![entry(PANE_LAYOUT, &oversized, 1)])
            .await
            .err(),
        Some(ErrorCode::InvalidArgument)
    );
    assert!(list(&fixture).await.is_empty());
}

#[tokio::test]
async fn the_key_cap_rejects_the_batch_that_would_exceed_it() {
    let fixture = WorkspacesFixture::new("client-state-key-cap").await;
    let fill: Vec<ClientStateEntry> = (0..MAX_KEYS_PER_ACCOUNT)
        .map(|index| entry(&format!("{DRAFT_PREFIX}k{index}"), "v0", 1))
        .collect();
    for batch in fill.chunks(MAX_BATCH_ENTRIES) {
        set(&fixture, batch.to_vec())
            .await
            .expect("stored within the cap");
    }
    assert_eq!(list(&fixture).await.len(), MAX_KEYS_PER_ACCOUNT);

    let existing_key = format!("{DRAFT_PREFIX}k0");
    let overflow_key = format!("{DRAFT_PREFIX}overflow");
    let overflow = vec![
        entry(&existing_key, "changed", 2),
        entry(&overflow_key, "x", 2),
    ];
    assert_eq!(
        set(&fixture, overflow).await.err(),
        Some(ErrorCode::ResourceExhausted)
    );

    let listed = list(&fixture).await;
    assert_eq!(listed.len(), MAX_KEYS_PER_ACCOUNT);
    let kept = listed
        .iter()
        .find(|row| row.key == existing_key)
        .expect("the existing key");
    assert_eq!(kept.value, "v0");
    assert!(listed.iter().all(|row| row.key != overflow_key));
}

#[tokio::test]
async fn a_machine_principal_cannot_read_or_write_client_state() {
    let fixture = WorkspacesFixture::new("client-state-machine").await;

    let refused_list = handle_client_state_list(
        &fixture.core,
        &machine_caller(),
        ClientStateListRequest::default(),
    )
    .await;
    assert!(refused_list.is_err());

    let refused_set = handle_client_state_set(
        &fixture.core,
        &machine_caller(),
        ClientStateSetRequest {
            entries: vec![entry(PANE_LAYOUT, "x", 1)],
            ..Default::default()
        },
    )
    .await;
    assert!(refused_set.is_err());
    assert!(list(&fixture).await.is_empty());
}

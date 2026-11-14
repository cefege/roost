//! Account-scoped client-state sync, driven through `ClientCore`'s public events
//! the way a host drives it: pull on link open, merge by last-writer-wins,
//! flush dirty keys on the sweep after a quiet period, and settle on the first
//! merge or the deadline.

use std::rc::Rc;

use roost_client_core::client::client_state::ClientStateEntry;
use roost_client_core::store::prefs::PrefDefaults;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::memory::FOLDER_ORDER_KEY;
use roost_client_core::{
    ClientCore, ClientEvent, ClientStateStore, Effect, KeyValueStore, MemoryClock,
    MemoryEpochClock, MemoryKeyValueStore, RpcCall, RpcResult,
};

const DRAFT_KEY: &str = "roost.composerDrafts.v1";
const LOCAL_ONLY_KEY: &str = "roost.someLocalOnly";
const RECENT_KEY: &str = "roost.sidebar.recent";

struct Fixture {
    local: Rc<MemoryKeyValueStore>,
    epoch: Rc<MemoryEpochClock>,
    storage: Rc<ClientStateStore>,
    core: ClientCore,
}

fn fixture(epoch_ms: i64) -> Fixture {
    let local = Rc::new(MemoryKeyValueStore::new());
    let epoch = Rc::new(MemoryEpochClock::starting_at(epoch_ms));
    let storage = Rc::new(ClientStateStore::new(local.clone(), epoch.clone()));
    let core = ClientCore::new(
        Rc::new(MemoryClock::new()),
        storage.clone(),
        "tab-client-state",
        &PrefDefaults::default(),
    );
    Fixture {
        local,
        epoch,
        storage,
        core,
    }
}

fn entry(key: &str, value: &str, updated_ms: i64) -> ClientStateEntry {
    ClientStateEntry {
        key: key.to_owned(),
        value: value.to_owned(),
        updated_ms,
    }
}

/// Dial, then open the socket. Returns the call id of the pull the open issued.
fn open_sync_link(fixture: &mut Fixture) -> u64 {
    fixture.core.handle(ClientEvent::DialRequested);
    let effects = fixture.core.handle(ClientEvent::SyncLinkOpened {
        generation: 1,
        socket_id: "sock-1".to_owned(),
        process_epoch: "epoch-1".to_owned(),
    });
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Rpc(RpcCall::ClientStateList { call_id }) => Some(*call_id),
            _ => None,
        })
        .expect("opening the sync link pulls client state")
}

fn answer_list(fixture: &mut Fixture, call_id: u64, entries: Vec<ClientStateEntry>) {
    fixture.core.handle(ClientEvent::RpcResultReceived(
        RpcResult::ClientStateListed { call_id, entries },
    ));
}

fn sweep(core: &mut ClientCore, now_ms: u64) -> Vec<Effect> {
    core.handle(ClientEvent::Sweep { now_ms })
}

fn set_batches(effects: &[Effect]) -> Vec<Vec<ClientStateEntry>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Rpc(RpcCall::ClientStateSet { entries, .. }) => Some(entries.clone()),
            _ => None,
        })
        .collect()
}

fn pull_id(effects: &[Effect]) -> u64 {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Rpc(RpcCall::ClientStateList { call_id }) => Some(*call_id),
            _ => None,
        })
        .expect("client-state pull")
}

fn notify_changed(fixture: &mut Fixture) -> Vec<Effect> {
    use roost_proto::buffa::Message;
    let frame = roost_proto::FirehoseFrame {
        frame: Some(roost_proto::firehose_frame::Frame::ClientStateChanged(
            Box::default(),
        )),
        ..Default::default()
    };
    let event = roost_client_core::sync::decode::decode_firehose(
        &frame.try_encode_to_vec().expect("encoded control"),
        roost_client_core::sync::decode::SyncFrameMeta { generation: 1 },
    )
    .expect("valid client-state control");
    fixture.core.handle(event)
}

#[test]
fn a_newer_remote_entry_is_written_locally_and_not_pushed_back() {
    let mut fixture = fixture(0);
    let call_id = open_sync_link(&mut fixture);
    answer_list(&mut fixture, call_id, vec![entry(DRAFT_KEY, "remote", 500)]);

    assert_eq!(fixture.local.get(DRAFT_KEY).as_deref(), Some("remote"));
    assert!(set_batches(&sweep(&mut fixture.core, 0)).is_empty());
    assert!(
        set_batches(&sweep(&mut fixture.core, 1_000)).is_empty(),
        "a merged remote value is not dirty, so it is not pushed back"
    );
}

#[test]
fn a_newer_local_write_is_pushed_after_the_quiet_period_and_an_older_remote_loses() {
    let mut fixture = fixture(100);
    fixture.storage.set(DRAFT_KEY, "local");
    let call_id = open_sync_link(&mut fixture);
    answer_list(&mut fixture, call_id, vec![entry(DRAFT_KEY, "old", 50)]);

    assert_eq!(fixture.local.get(DRAFT_KEY).as_deref(), Some("local"));
    assert!(set_batches(&sweep(&mut fixture.core, 0)).is_empty());
    assert_eq!(
        set_batches(&sweep(&mut fixture.core, 1_000)),
        vec![vec![entry(DRAFT_KEY, "local", 100)]]
    );
}

#[test]
fn an_equal_timestamp_keeps_the_local_value() {
    let mut fixture = fixture(100);
    fixture.storage.set(DRAFT_KEY, "local");
    let call_id = open_sync_link(&mut fixture);
    answer_list(&mut fixture, call_id, vec![entry(DRAFT_KEY, "remote", 100)]);

    assert_eq!(fixture.local.get(DRAFT_KEY).as_deref(), Some("local"));
}

#[test]
fn a_key_outside_the_allowlist_is_never_flushed() {
    let mut fixture = fixture(100);
    fixture.storage.set(LOCAL_ONLY_KEY, "private");
    assert_eq!(
        fixture.local.get(LOCAL_ONLY_KEY).as_deref(),
        Some("private")
    );

    assert!(set_batches(&sweep(&mut fixture.core, 0)).is_empty());
    assert!(set_batches(&sweep(&mut fixture.core, 5_000)).is_empty());
}

#[test]
fn three_writes_inside_the_quiet_window_flush_as_one_entry() {
    let mut fixture = fixture(100);
    fixture.storage.set(DRAFT_KEY, "first");
    assert!(set_batches(&sweep(&mut fixture.core, 100)).is_empty());

    fixture.epoch.set(150);
    fixture.storage.set(DRAFT_KEY, "second");
    assert!(set_batches(&sweep(&mut fixture.core, 600)).is_empty());

    fixture.epoch.set(300);
    fixture.storage.set(DRAFT_KEY, "third");
    assert!(set_batches(&sweep(&mut fixture.core, 1_000)).is_empty());
    assert!(
        set_batches(&sweep(&mut fixture.core, 1_500)).is_empty(),
        "still inside the quiet period after the last write"
    );
    assert_eq!(
        set_batches(&sweep(&mut fixture.core, 2_000)),
        vec![vec![entry(DRAFT_KEY, "third", 300)]]
    );
}

#[test]
fn removing_a_synced_key_flushes_an_empty_value() {
    let mut fixture = fixture(100);
    fixture.storage.set(DRAFT_KEY, "present");
    fixture.epoch.set(200);
    fixture.storage.remove(DRAFT_KEY);
    assert_eq!(fixture.local.get(DRAFT_KEY), None);

    assert!(set_batches(&sweep(&mut fixture.core, 0)).is_empty());
    assert_eq!(
        set_batches(&sweep(&mut fixture.core, 1_000)),
        vec![vec![entry(DRAFT_KEY, "", 200)]],
        "a removal syncs as the empty value; the server cannot store a tombstone"
    );
}

#[test]
fn an_empty_remote_value_removes_the_local_key() {
    let mut fixture = fixture(100);
    fixture.storage.set(DRAFT_KEY, "present");
    let call_id = open_sync_link(&mut fixture);
    answer_list(&mut fixture, call_id, vec![entry(DRAFT_KEY, "", 300)]);

    assert_eq!(fixture.local.get(DRAFT_KEY), None);
}

#[test]
fn settled_flips_on_the_first_merge() {
    let mut fixture = fixture(0);
    assert!(!fixture.core.client_state_settled());
    let call_id = open_sync_link(&mut fixture);
    answer_list(&mut fixture, call_id, Vec::new());

    assert!(fixture.core.client_state_settled());
}

#[test]
fn settled_flips_on_the_deadline_when_no_pull_answers() {
    let mut fixture = fixture(0);
    let _call_id = open_sync_link(&mut fixture);

    sweep(&mut fixture.core, 4_999);
    assert!(!fixture.core.client_state_settled());

    sweep(&mut fixture.core, 5_000);
    assert!(fixture.core.client_state_settled());
}

#[test]
fn a_remote_list_merge_updates_the_stores_without_reload() {
    let mut fixture = fixture(0);
    let call_id = open_sync_link(&mut fixture);
    answer_list(
        &mut fixture,
        call_id,
        vec![entry(RECENT_KEY, r#"["remote-session"]"#, 500)],
    );

    assert_eq!(
        fixture.core.store().sidebar.memory.recent(),
        &["remote-session".to_owned()][..],
        "a merged remote value is visible through the hydrated store at once"
    );
}

#[test]
fn an_equal_timestamp_on_a_dirty_key_is_flushed_not_overwritten() {
    let mut fixture = fixture(100);
    fixture.storage.set(DRAFT_KEY, "local");
    let call_id = open_sync_link(&mut fixture);
    answer_list(&mut fixture, call_id, vec![entry(DRAFT_KEY, "remote", 100)]);

    assert_eq!(fixture.local.get(DRAFT_KEY).as_deref(), Some("local"));
    assert!(set_batches(&sweep(&mut fixture.core, 0)).is_empty());
    assert_eq!(
        set_batches(&sweep(&mut fixture.core, 1_000)),
        vec![vec![entry(DRAFT_KEY, "local", 100)]],
        "a tie goes to the incoming write on the coordinator, so the dirty local value is pushed"
    );
}

#[test]
fn a_folder_drag_reaches_an_open_device_without_echoing() {
    let mut writer = fixture(500);
    let mut reader = fixture(1_000);
    for device in [&mut writer, &mut reader] {
        let call_id = open_sync_link(device);
        answer_list(device, call_id, Vec::new());
        device
            .core
            .handle(ClientEvent::Sidebar(SidebarIntent::ObserveFolders(vec![
                "a".into(),
                "b".into(),
                "c".into(),
            ])));
    }
    writer
        .core
        .handle(ClientEvent::Sidebar(SidebarIntent::MoveFolder {
            source: "c".into(),
            target: "a".into(),
        }));
    sweep(&mut writer.core, 0);
    let batches = set_batches(&sweep(&mut writer.core, 1_000));
    assert_eq!(
        batches,
        vec![vec![entry(FOLDER_ORDER_KEY, r#"["c","a","b"]"#, 500)]]
    );
    let call_id = pull_id(&notify_changed(&mut reader));
    answer_list(&mut reader, call_id, batches[0].clone());
    assert_eq!(
        reader.core.store().sidebar.memory.folder_order(),
        ["c", "a", "b"]
    );
    assert!(set_batches(&sweep(&mut reader.core, 0)).is_empty());
    assert!(set_batches(&sweep(&mut reader.core, 1_000)).is_empty());
    assert_eq!(
        roost_client_core::store::sidebar::SidebarMemory::load(reader.storage.as_ref())
            .folder_order(),
        ["c", "a", "b"],
    );
}

#[test]
fn discovering_folders_before_the_first_pull_cannot_overwrite_the_account_order() {
    let mut fixture = fixture(1_000);
    let call_id = open_sync_link(&mut fixture);
    fixture
        .core
        .handle(ClientEvent::Sidebar(SidebarIntent::ObserveFolders(vec![
            "c".into(),
            "a".into(),
            "b".into(),
        ])));
    answer_list(
        &mut fixture,
        call_id,
        vec![entry(FOLDER_ORDER_KEY, r#"["b","a"]"#, 500)],
    );
    assert_eq!(
        fixture.core.store().sidebar.memory.folder_order(),
        ["b", "a", "c"]
    );
    assert!(set_batches(&sweep(&mut fixture.core, 0)).is_empty());
    assert!(set_batches(&sweep(&mut fixture.core, 1_000)).is_empty());
}

#[test]
fn an_unstamped_browser_order_seeds_only_an_account_without_an_order() {
    for remote in [
        Vec::new(),
        vec![entry(FOLDER_ORDER_KEY, r#"["a","b"]"#, 500)],
    ] {
        let mut fixture = fixture(1_000);
        fixture.local.set(FOLDER_ORDER_KEY, r#"["b","a"]"#);
        let call_id = open_sync_link(&mut fixture);
        let existing = !remote.is_empty();
        answer_list(&mut fixture, call_id, remote);
        sweep(&mut fixture.core, 0);
        let batches = set_batches(&sweep(&mut fixture.core, 1_000));
        if existing {
            assert_eq!(
                fixture.local.get(FOLDER_ORDER_KEY).as_deref(),
                Some(r#"["a","b"]"#)
            );
            assert!(batches.is_empty());
        } else {
            assert_eq!(
                batches,
                vec![vec![entry(FOLDER_ORDER_KEY, r#"["b","a"]"#, 1_000)]]
            );
        }
    }
}

#[test]
fn a_change_during_a_pull_is_coalesced_but_not_lost() {
    let mut fixture = fixture(1_000);
    let first_pull = open_sync_link(&mut fixture);
    assert!(notify_changed(&mut fixture).is_empty());
    assert!(notify_changed(&mut fixture).is_empty());
    answer_list(&mut fixture, first_pull, Vec::new());
    let second_pull = pull_id(&sweep(&mut fixture.core, 0));
    answer_list(
        &mut fixture,
        second_pull,
        vec![entry(FOLDER_ORDER_KEY, r#"["b","a"]"#, 500)],
    );
    assert_eq!(
        fixture.core.store().sidebar.memory.folder_order(),
        ["b", "a"]
    );
    assert!(
        sweep(&mut fixture.core, 1)
            .iter()
            .all(|effect| !matches!(effect, Effect::Rpc(RpcCall::ClientStateList { .. })))
    );
}

//! Tab identity arbitration: one tab id per document, a duplicated tab rotated
//! off its sibling's id, and a browser without arbitration keeping its id.
//! Ports `apps/web/tests/tabIdentity.test.ts`; the fakes below play the
//! browser half `roost-web`'s `platform::tab_id` performs.

use std::collections::BTreeSet;
use std::rc::Rc;

use roost_client_core::client::auth::CountingRandomSource;
use roost_client_core::client::auth::tab_id::{
    ArbitrationPrimitives, ClaimMessage, ClaimStep, LockAttempt, TAB_ID_KEY, TabIdentity,
};
use roost_client_core::{KeyValueStore, MemoryClock, MemoryKeyValueStore};

/// v2 `FakeLockManager`: an `ifAvailable` request is granted unless this
/// document or another holds the name; `denied` is a `request` that throws.
#[derive(Default)]
struct FakeLocks {
    held: BTreeSet<String>,
    externally_held: BTreeSet<String>,
    denied: bool,
}

impl FakeLocks {
    fn request(&mut self, name: &str) -> LockAttempt {
        if self.denied {
            return LockAttempt::Unavailable;
        }
        if self.held.contains(name) || self.externally_held.contains(name) {
            return LockAttempt::Occupied;
        }
        self.held.insert(name.to_owned());
        LockAttempt::Acquired
    }
}

/// v2 `FakeBroadcastChannel`: an owner of an `occupied_ids` id answers the
/// probe; a `probing_ids` id is probed back once by a concurrent document.
#[derive(Default)]
struct FakeChannel {
    occupied_ids: BTreeSet<String>,
    probing_ids: BTreeSet<String>,
}

impl FakeChannel {
    /// What another document posts back after `message`.
    fn post(&mut self, message: &ClaimMessage) -> Option<ClaimMessage> {
        let ClaimMessage::Probe { id, nonce } = message else {
            return None;
        };
        if self.occupied_ids.contains(id) {
            return Some(ClaimMessage::Occupied {
                id: id.clone(),
                nonce: nonce.clone(),
            });
        }
        self.probing_ids.remove(id).then(|| ClaimMessage::Probe {
            id: id.clone(),
            nonce: "peer-probe".to_owned(),
        })
    }
}

fn session_storage(stored: &str) -> Rc<MemoryKeyValueStore> {
    let storage = Rc::new(MemoryKeyValueStore::new());
    storage.set(TAB_ID_KEY, stored);
    storage
}

fn document(storage: &Rc<MemoryKeyValueStore>, entropy: u8) -> TabIdentity {
    TabIdentity::new(
        Rc::clone(storage) as Rc<dyn KeyValueStore>,
        Rc::new(CountingRandomSource::new(entropy)),
        Rc::new(MemoryClock::new()),
    )
}

/// Drive a claim the way the browser host does: a lock answer, a delivered
/// reply, or the probe window elapsing with no reply.
fn claim(
    identity: &mut TabIdentity,
    mut locks: Option<&mut FakeLocks>,
    mut channel: Option<&mut FakeChannel>,
) -> (String, bool) {
    let mut step = identity.begin_claim(ArbitrationPrimitives {
        web_locks: locks.is_some(),
        broadcast_channel: channel.is_some(),
    });
    for _ in 0..16 {
        step = match step {
            ClaimStep::RequestLock { name } => {
                let locks = locks.as_deref_mut().expect("a lock step needs Web Locks");
                identity
                    .lock_answered(locks.request(&name))
                    .expect("a lock was requested")
            }
            ClaimStep::Probe { message } => {
                let channel = channel
                    .as_deref_mut()
                    .expect("a probe step needs a channel");
                channel
                    .post(&message)
                    .and_then(|reply| identity.message_received(reply).step)
                    .or_else(|| identity.probe_timed_out(message.nonce()))
                    .expect("the pending probe settles")
            }
            ClaimStep::Claimed { id, keep_channel } => return (id, keep_channel),
        };
    }
    panic!("the claim did not settle");
}

#[test]
fn holds_one_web_lock_and_preserves_the_id_across_a_reload() {
    let storage = session_storage("stable-tab");
    let mut locks = FakeLocks::default();

    let mut first = document(&storage, 1);
    let (claimed, _) = claim(&mut first, Some(&mut locks), None);
    assert_eq!(claimed, "stable-tab");
    assert_eq!(first.tab_id(), "stable-tab");
    assert!(locks.held.contains("roost.tab-id:stable-tab"));

    // A reload tears the document down, which releases its lock, and the next
    // document reads the same sessionStorage.
    drop(first);
    locks.held.clear();
    let mut reloaded = document(&storage, 1);
    let (claimed, _) = claim(&mut reloaded, Some(&mut locks), None);
    assert_eq!(claimed, "stable-tab");
    assert!(locks.held.contains("roost.tab-id:stable-tab"));
}

#[test]
fn rotates_only_a_duplicated_web_lock_identity() {
    let storage = session_storage("copied-tab");
    let mut locks = FakeLocks::default();
    locks
        .externally_held
        .insert("roost.tab-id:copied-tab".to_owned());

    let mut identity = document(&storage, 1);
    let (claimed, _) = claim(&mut identity, Some(&mut locks), None);
    assert_ne!(claimed, "copied-tab");
    assert_eq!(identity.tab_id(), claimed);
    assert_eq!(storage.get(TAB_ID_KEY).as_deref(), Some(claimed.as_str()));
    assert!(locks.externally_held.contains("roost.tab-id:copied-tab"));
    assert!(locks.held.contains(&format!("roost.tab-id:{claimed}")));
}

#[test]
fn rotates_when_the_broadcast_fallback_reports_an_owner() {
    let storage = session_storage("copied-broadcast-tab");
    let mut channel = FakeChannel::default();
    channel
        .occupied_ids
        .insert("copied-broadcast-tab".to_owned());

    let mut identity = document(&storage, 1);
    let (claimed, keep_channel) = claim(&mut identity, None, Some(&mut channel));
    assert_ne!(claimed, "copied-broadcast-tab");
    assert_eq!(storage.get(TAB_ID_KEY).as_deref(), Some(claimed.as_str()));
    assert!(
        keep_channel,
        "the owner's channel stays open to answer probes"
    );
}

#[test]
fn rotates_when_another_broadcast_document_probes_concurrently() {
    let storage = session_storage("concurrently-copied-tab");
    let mut channel = FakeChannel::default();
    channel
        .probing_ids
        .insert("concurrently-copied-tab".to_owned());

    let mut identity = document(&storage, 1);
    let (claimed, _) = claim(&mut identity, None, Some(&mut channel));
    assert_ne!(claimed, "concurrently-copied-tab");
    assert_eq!(storage.get(TAB_ID_KEY).as_deref(), Some(claimed.as_str()));
}

#[test]
fn falls_back_after_web_locks_throws_without_blocking_startup() {
    let storage = session_storage("locks-denied-tab");
    let mut locks = FakeLocks {
        denied: true,
        ..FakeLocks::default()
    };

    let mut identity = document(&storage, 1);
    let (claimed, _) = claim(&mut identity, Some(&mut locks), None);
    assert_eq!(claimed, "locks-denied-tab");
}

#[test]
fn degrades_without_rotating_when_no_arbitration_primitive_exists() {
    let storage = session_storage("unsupported-browser-tab");

    let mut identity = document(&storage, 1);
    let (claimed, keep_channel) = claim(&mut identity, None, None);
    assert_eq!(claimed, "unsupported-browser-tab");
    assert!(!keep_channel);
}

#[test]
fn a_duplicated_document_rotates_off_a_live_broadcast_owner() {
    let mut owner = document(&session_storage("shared-tab"), 1);
    let (owned, keep_channel) = claim(&mut owner, None, Some(&mut FakeChannel::default()));
    assert_eq!(owned, "shared-tab");
    assert!(keep_channel);

    // Duplication copies sessionStorage; the owner's open channel answers.
    let storage = session_storage("shared-tab");
    let mut duplicate = document(&storage, 100);
    let mut step = duplicate.begin_claim(ArbitrationPrimitives {
        web_locks: false,
        broadcast_channel: true,
    });
    let claimed = loop {
        step = match step {
            ClaimStep::Probe { message } => owner
                .message_received(message.clone())
                .reply
                .and_then(|reply| duplicate.message_received(reply).step)
                .or_else(|| duplicate.probe_timed_out(message.nonce()))
                .expect("the pending probe settles"),
            ClaimStep::Claimed { id, .. } => break id,
            ClaimStep::RequestLock { .. } => panic!("no Web Locks in this document"),
        };
    };
    assert_ne!(claimed, "shared-tab");
    assert_eq!(storage.get(TAB_ID_KEY).as_deref(), Some(claimed.as_str()));
    assert_eq!(owner.tab_id(), "shared-tab");
}

#[test]
fn a_fresh_document_mints_a_v4_uuid_and_stores_it() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut identity = document(&storage, 7);
    let minted = identity.tab_id();

    let groups: Vec<usize> = minted.split('-').map(str::len).collect();
    assert_eq!(groups, [8, 4, 4, 4, 12], "{minted}");
    assert!(
        minted
            .chars()
            .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(minted.chars().nth(14), Some('4'), "{minted}");
    assert!(
        matches!(minted.chars().nth(19), Some('8' | '9' | 'a' | 'b')),
        "{minted}"
    );
    assert_eq!(storage.get(TAB_ID_KEY).as_deref(), Some(minted.as_str()));
    assert_eq!(identity.tab_id(), minted, "one document, one id");
}

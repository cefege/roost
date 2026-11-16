//! Last-writer-wins merge of coordinator answers into the local store. One rule
//! serves both List and Set: the newer stamp wins, a local win is queued for the
//! next flush, and an equal stamp is a tie the coordinator resolves toward the
//! incoming write, so a local value still awaiting its push is kept and pushed.

use std::collections::BTreeSet;

use roost_protocol::client_state::{is_synced_key, theme};

use super::entry::ClientStateEntry;
use super::store::ClientStateStore;
use crate::platform::KeyValueStore;
use crate::store::sidebar::memory::FOLDER_ORDER_KEY;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct MergeCounts {
    /// Remote values written into local storage.
    pub(crate) applied: usize,
    /// Local values that are newer and now queued for a push.
    pub(crate) queued: usize,
    /// Entries for keys outside the synced allowlist, dropped.
    pub(crate) ignored: usize,
}

/// Merge the entries a Set answered with. Set answers carry every requested key,
/// so no key outside them is considered.
pub(crate) fn merge_remote_entries(
    storage: &ClientStateStore,
    entries: &[ClientStateEntry],
) -> MergeCounts {
    let mut counts = MergeCounts::default();
    for entry in entries {
        if !is_synced_key(&entry.key) {
            counts.ignored += 1;
            continue;
        }
        let local_stamp = storage.stamp_of(&entry.key);
        if local_stamp.is_some_and(|local| local > entry.updated_ms) {
            storage.mark_dirty(&entry.key);
            counts.queued += 1;
        } else if local_stamp == Some(entry.updated_ms) {
            merge_equal_stamp(storage, entry, &mut counts);
        } else {
            storage.apply_remote(entry);
            counts.applied += 1;
        }
    }
    if counts.ignored > 0 {
        tracing::warn!(
            target: "client_state",
            ignored = counts.ignored,
            "client state answer carried non-synced keys; ignored"
        );
    }
    counts
}

/// The coordinator accepts a write at the stored stamp, so a tie goes to the
/// incoming value. A local value still awaiting its push is the write the
/// coordinator is about to receive, so it keeps its place and flushes.
fn merge_equal_stamp(
    storage: &ClientStateStore,
    entry: &ClientStateEntry,
    counts: &mut MergeCounts,
) {
    if storage.is_dirty(&entry.key) {
        counts.queued += 1;
    } else if storage.get(&entry.key).unwrap_or_default() != entry.value {
        storage.apply_remote(entry);
        counts.applied += 1;
    }
}

/// Merge a full List answer. A List carries every key the account has, so a
/// locally stamped key it omits was never pushed and is queued now.
pub(crate) fn merge_list_answer(
    storage: &ClientStateStore,
    entries: &[ClientStateEntry],
    first_list: bool,
) -> MergeCounts {
    let mut counts = merge_remote_entries(storage, entries);
    let remote_keys: BTreeSet<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
    if first_list && !theme::KEYS.iter().any(|key| remote_keys.contains(key)) {
        for key in theme::KEYS {
            if storage.stamp_of(key).is_none()
                && let Some(value) = storage.get(key)
            {
                storage.set(key, &value);
            }
        }
    }
    // An unstamped order predates account sync. Seed only an empty account;
    // another device's saved order must win over a browser merely opening.
    if !remote_keys.contains(FOLDER_ORDER_KEY)
        && storage.stamp_of(FOLDER_ORDER_KEY).is_none()
        && let Some(value) = storage.get(FOLDER_ORDER_KEY)
        && serde_json::from_str::<Vec<String>>(&value).is_ok_and(|keys| !keys.is_empty())
    {
        storage.set(FOLDER_ORDER_KEY, &value);
    }
    for key in storage.stamped_keys() {
        if is_synced_key(&key) && !remote_keys.contains(key.as_str()) {
            storage.mark_dirty(&key);
            counts.queued += 1;
        }
    }
    counts
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::rc::Rc;

    use crate::store::prefs::PrefDefaults;
    use crate::{
        ClientCore, ClientEvent, Effect, MemoryClock, MemoryEpochClock, MemoryKeyValueStore,
        RpcCall, RpcResult,
    };

    use super::*;

    fn theme_store(epoch_ms: i64) -> (Rc<MemoryKeyValueStore>, ClientStateStore) {
        let local = Rc::new(MemoryKeyValueStore::new());
        let epoch = Rc::new(MemoryEpochClock::starting_at(epoch_ms));
        let storage = ClientStateStore::new(local.clone(), epoch);
        (local, storage)
    }

    fn theme_entry(key: &str, value: &str, updated_ms: i64) -> ClientStateEntry {
        ClientStateEntry {
            key: key.to_owned(),
            value: value.to_owned(),
            updated_ms,
        }
    }

    #[test]
    fn theme_changes_reach_another_device_without_echo() {
        let (_, writer) = theme_store(500);
        let (reader_local, reader) = theme_store(1_000);
        merge_list_answer(&writer, &[], true);
        merge_list_answer(&reader, &[], true);
        for (key, value) in theme::KEYS.iter().zip(["neutral", "blue", "dark"]) {
            writer.set(key, value);
        }
        let entries: Vec<_> = theme::KEYS
            .iter()
            .map(|key| writer.flush_entry(key).unwrap())
            .collect();
        merge_list_answer(&reader, &entries, false);
        for (key, value) in theme::KEYS.iter().zip(["neutral", "blue", "dark"]) {
            assert_eq!(reader_local.get(key).as_deref(), Some(value));
            assert!(!reader.is_dirty(key));
        }
    }

    #[test]
    fn unstamped_theme_seeds_only_an_account_without_any_theme_key() {
        for remote in [
            Vec::new(),
            vec![theme_entry(theme::BASE_STORAGE_KEY, "remote", 500)],
        ] {
            let (local, storage) = theme_store(1_000);
            local.set(theme::BASE_STORAGE_KEY, "local");
            local.set(theme::ACCENT_STORAGE_KEY, "amber");
            let existing = !remote.is_empty();
            merge_list_answer(&storage, &remote, true);
            if existing {
                assert_eq!(
                    local.get(theme::BASE_STORAGE_KEY).as_deref(),
                    Some("remote")
                );
                assert!(!storage.is_dirty(theme::BASE_STORAGE_KEY));
                assert!(!storage.is_dirty(theme::ACCENT_STORAGE_KEY));
                merge_list_answer(&storage, &[], false);
                assert!(!storage.is_dirty(theme::ACCENT_STORAGE_KEY));
            } else {
                assert_eq!(
                    storage.flush_entry(theme::BASE_STORAGE_KEY),
                    Some(theme_entry(theme::BASE_STORAGE_KEY, "local", 1_000))
                );
                assert_eq!(
                    storage.flush_entry(theme::ACCENT_STORAGE_KEY),
                    Some(theme_entry(theme::ACCENT_STORAGE_KEY, "amber", 1_000))
                );
                assert!(storage.is_dirty(theme::BASE_STORAGE_KEY));
                assert!(storage.is_dirty(theme::ACCENT_STORAGE_KEY));
            }
        }
    }

    #[test]
    fn later_theme_edits_survive_a_reconnect_and_are_queued() {
        let (local, storage) = theme_store(1_000);
        merge_list_answer(
            &storage,
            &[theme_entry(theme::MODE_STORAGE_KEY, "light", 500)],
            true,
        );
        storage.set(theme::MODE_STORAGE_KEY, "dark");
        merge_list_answer(
            &storage,
            &[theme_entry(theme::MODE_STORAGE_KEY, "light", 500)],
            false,
        );
        assert_eq!(local.get(theme::MODE_STORAGE_KEY).as_deref(), Some("dark"));
        assert!(storage.is_dirty(theme::MODE_STORAGE_KEY));
        assert_eq!(
            storage.flush_entry(theme::MODE_STORAGE_KEY),
            Some(theme_entry(theme::MODE_STORAGE_KEY, "dark", 1_000))
        );
    }

    #[test]
    fn theme_writes_flow_through_client_core_to_another_device() {
        let (_, writer_storage) = theme_store(500);
        let (_, reader_storage) = theme_store(1_000);
        let writer_storage = Rc::new(writer_storage);
        let reader_storage = Rc::new(reader_storage);
        let mut writer = ClientCore::new(
            Rc::new(MemoryClock::new()),
            writer_storage.clone(),
            "writer",
            &PrefDefaults::default(),
        );
        let mut reader = ClientCore::new(
            Rc::new(MemoryClock::new()),
            reader_storage.clone(),
            "reader",
            &PrefDefaults::default(),
        );
        let mut call_ids = Vec::new();
        for core in [&mut writer, &mut reader] {
            core.handle(ClientEvent::DialRequested);
            call_ids.push(
                core.handle(ClientEvent::SyncLinkOpened {
                    generation: 1,
                    socket_id: "socket".into(),
                    process_epoch: "epoch".into(),
                })
                .into_iter()
                .find_map(|effect| match effect {
                    Effect::Rpc(RpcCall::ClientStateList { call_id }) => Some(call_id),
                    _ => None,
                })
                .unwrap(),
            );
        }
        writer.handle(ClientEvent::RpcResultReceived(
            RpcResult::ClientStateListed {
                call_id: call_ids[0],
                entries: Vec::new(),
            },
        ));
        assert_eq!(reader.store().client_state.merged_revision(), 0);
        writer_storage.set(theme::MODE_STORAGE_KEY, "dark");
        writer.handle(ClientEvent::Sweep { now_ms: 0 });
        let entries = writer
            .handle(ClientEvent::Sweep { now_ms: 1_000 })
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Rpc(RpcCall::ClientStateSet { entries, .. }) => Some(entries),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            entries,
            vec![theme_entry(theme::MODE_STORAGE_KEY, "dark", 500)]
        );
        reader.handle(ClientEvent::RpcResultReceived(
            RpcResult::ClientStateListed {
                call_id: call_ids[1],
                entries,
            },
        ));
        assert_eq!(reader.store().client_state.merged_revision(), 1);
        assert_eq!(
            reader_storage.get(theme::MODE_STORAGE_KEY).as_deref(),
            Some("dark")
        );
        reader.handle(ClientEvent::Sweep { now_ms: 0 });
        assert!(
            !reader
                .handle(ClientEvent::Sweep { now_ms: 1_000 })
                .iter()
                .any(|effect| matches!(effect, Effect::Rpc(RpcCall::ClientStateSet { .. })))
        );
    }

    #[test]
    fn retired_local_stamps_cannot_block_a_supported_preference_upload() {
        let local = Rc::new(MemoryKeyValueStore::new());
        let epoch = Rc::new(MemoryEpochClock::starting_at(100));
        let order = r#"["worker:/second","worker:/first"]"#;
        {
            let previous = ClientStateStore::new(local.clone(), epoch.clone());
            for (key, value) in [
                ("roost.agent.draft.c-retired", "unsent draft"),
                (FOLDER_ORDER_KEY, order),
            ] {
                previous.apply_remote(&ClientStateEntry {
                    key: key.to_owned(),
                    value: value.to_owned(),
                    updated_ms: 100,
                });
            }
        }
        let storage = Rc::new(ClientStateStore::new(local, epoch));
        let mut core = ClientCore::new(
            Rc::new(MemoryClock::new()),
            storage,
            "tab-upgraded",
            &PrefDefaults::default(),
        );
        core.handle(ClientEvent::DialRequested);
        let call_id = core
            .handle(ClientEvent::SyncLinkOpened {
                generation: 1,
                socket_id: "socket-upgraded".to_owned(),
                process_epoch: "epoch-upgraded".to_owned(),
            })
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Rpc(RpcCall::ClientStateList { call_id }) => Some(call_id),
                _ => None,
            })
            .unwrap();
        core.handle(ClientEvent::RpcResultReceived(
            RpcResult::ClientStateListed {
                call_id,
                entries: Vec::new(),
            },
        ));
        core.handle(ClientEvent::Sweep { now_ms: 0 });
        let batches: Vec<_> = core
            .handle(ClientEvent::Sweep { now_ms: 1_000 })
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Rpc(RpcCall::ClientStateSet { entries, .. }) => Some(entries),
                _ => None,
            })
            .collect();
        assert_eq!(
            batches,
            vec![vec![ClientStateEntry {
                key: FOLDER_ORDER_KEY.to_owned(),
                value: order.to_owned(),
                updated_ms: 100,
            }]]
        );
    }
}

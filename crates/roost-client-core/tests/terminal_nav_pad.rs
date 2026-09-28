//! The terminal key pad's store seam: a pad with no pointer must be able to
//! read whether the sheet is open, close it (which is the ONLY way to drop a
//! latched Ctrl), and every change must repaint and persist.
//! Ports the store half of `apps/web/tests/terminalNavButtons.padSeam.dom.test.ts`.

use std::cell::RefCell;

use roost_client_core::store::terminal_nav_pad::{
    TERMINAL_NAV_PAD_OPEN_KEY, close_terminal_nav_pad, load_terminal_nav_pad, terminal_nav_pad_open,
    toggle_terminal_nav_pad,
};
use roost_client_core::{ClientCore, KeyValueStore, MemoryKeyValueStore};

/// Storage that records every write, so "no write" is observable.
#[derive(Default)]
struct RecordingStorage {
    inner: MemoryKeyValueStore,
    writes: RefCell<Vec<String>>,
}

impl KeyValueStore for RecordingStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.inner.get(key)
    }

    fn set(&self, key: &str, value: &str) {
        self.writes.borrow_mut().push(format!("{key}={value}"));
        self.inner.set(key, value);
    }

    fn remove(&self, key: &str) {
        self.inner.remove(key);
    }
}

fn client() -> ClientCore {
    ClientCore::in_memory("tab-nav-pad")
}

#[test]
fn toggling_repaints_and_persists_each_way() {
    let mut core = client();
    let storage = RecordingStorage::default();
    assert!(!terminal_nav_pad_open(core.store()));

    let before = core.store().revision();
    assert!(toggle_terminal_nav_pad(core.store_mut(), &storage));
    // A snapshot the renderer cannot see move would leave the sheet stale.
    assert!(core.store().revision() > before);
    assert!(terminal_nav_pad_open(core.store()));
    assert_eq!(storage.get(TERMINAL_NAV_PAD_OPEN_KEY).as_deref(), Some("1"));

    assert!(!toggle_terminal_nav_pad(core.store_mut(), &storage));
    assert!(!terminal_nav_pad_open(core.store()));
    assert_eq!(storage.get(TERMINAL_NAV_PAD_OPEN_KEY).as_deref(), Some("0"));
}

#[test]
fn close_no_ops_when_closed_and_disarms_a_real_close() {
    let mut core = client();
    let storage = RecordingStorage::default();
    let revision = core.store().revision();

    assert!(!close_terminal_nav_pad(core.store_mut(), &storage));
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 0);
    assert_eq!(*storage.writes.borrow(), Vec::<String>::new());
    assert_eq!(core.store().revision(), revision);

    toggle_terminal_nav_pad(core.store_mut(), &storage);
    // Opening must not run the close path's disarm.
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 0);

    assert!(close_terminal_nav_pad(core.store_mut(), &storage));
    assert!(!terminal_nav_pad_open(core.store()));
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 1);
}

#[test]
fn a_toggle_close_runs_the_same_disarm_path() {
    let mut core = client();
    let storage = RecordingStorage::default();

    toggle_terminal_nav_pad(core.store_mut(), &storage);
    toggle_terminal_nav_pad(core.store_mut(), &storage);

    assert!(!terminal_nav_pad_open(core.store()));
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 1);
}

#[test]
fn a_reload_restores_only_an_explicitly_open_pad() {
    let storage = RecordingStorage::default();
    let mut core = client();
    assert!(!load_terminal_nav_pad(core.store_mut(), &storage), "nothing stored, nothing changes");

    storage.set(TERMINAL_NAV_PAD_OPEN_KEY, "1");
    let mut reloaded = client();
    assert!(load_terminal_nav_pad(reloaded.store_mut(), &storage));
    assert!(terminal_nav_pad_open(reloaded.store()));

    storage.set(TERMINAL_NAV_PAD_OPEN_KEY, "true");
    let mut corrupt = client();
    assert!(!load_terminal_nav_pad(corrupt.store_mut(), &storage));
    assert!(!terminal_nav_pad_open(corrupt.store()));
}

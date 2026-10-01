//! The shell's key-pad and warning actions through `ClientCore::handle`, over
//! the one fold every shell action takes.
//!
//! The invariant the key sheet depends on: an intent that moved something bumps
//! the revision exactly once (so the sheet repaints once), persists what
//! persists, and a close runs the store's single disarm path — so reopening
//! never brings a latched Ctrl back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;

use roost_client_core::store::prefs::PrefDefaults;
use roost_client_core::store::shell_intent::{SHELL_WARNING_TOAST, ShellIntent};
use roost_client_core::store::terminal_nav_pad::{
    TERMINAL_NAV_PAD_OPEN_KEY, terminal_nav_pad_open,
};
use roost_client_core::store::toasts::ToastKind;
use roost_client_core::store::toasts::ToastSource;
use roost_client_core::{ClientCore, ClientEvent, KeyValueStore, MemoryClock, MemoryKeyValueStore};

fn core_over(storage: &Rc<MemoryKeyValueStore>) -> ClientCore {
    ClientCore::new(
        Rc::new(MemoryClock::new()),
        Rc::clone(storage) as Rc<dyn KeyValueStore>,
        "tab-shell-nav-pad",
        &PrefDefaults::default(),
    )
}

/// Whether the intent moved the store, read the way the host sees it.
fn shell(core: &mut ClientCore, intent: ShellIntent) -> bool {
    let before = core.store().revision();
    core.handle(ClientEvent::Shell(intent));
    core.store().revision() > before
}

#[test]
fn the_key_pad_toggles_through_one_store_path_and_persists_both_ways() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);

    assert!(shell(&mut core, ShellIntent::ToggleNavPad));
    assert!(terminal_nav_pad_open(core.store()));
    assert_eq!(storage.get(TERMINAL_NAV_PAD_OPEN_KEY).as_deref(), Some("1"));
    assert!(
        core_over(&storage).store().terminal_nav_pad.is_open(),
        "a reload must show the sheet the reader left open"
    );

    assert!(shell(&mut core, ShellIntent::ToggleNavPad));
    assert!(!terminal_nav_pad_open(core.store()));
    assert_eq!(storage.get(TERMINAL_NAV_PAD_OPEN_KEY).as_deref(), Some("0"));
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 1);
}

#[test]
fn a_close_disarms_a_real_close_once_and_then_never_again() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);
    shell(&mut core, ShellIntent::ToggleNavPad);
    // Opening must not run the close path's disarm.
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 0);

    assert!(shell(&mut core, ShellIntent::CloseNavPad));
    assert!(!terminal_nav_pad_open(core.store()));
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 1);
    let revision = core.store().revision();

    // A second close must not disarm a Ctrl the reader armed from the sheet's
    // own toggle a moment earlier.
    assert!(!shell(&mut core, ShellIntent::CloseNavPad));
    assert_eq!(core.store().revision(), revision);
    assert_eq!(core.store().terminal_nav_pad.disarm_count(), 1);
}

#[test]
fn a_warning_raises_one_card_under_the_shell_source() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);

    assert!(shell(
        &mut core,
        ShellIntent::ShowWarning {
            message: "Tap the mic once to allow it.".to_owned(),
        }
    ));
    let cards = core.store().toasts.toasts().collect::<Vec<_>>();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].msg, "Tap the mic once to allow it.");
    assert_eq!(cards[0].kind, ToastKind::Warn);
    assert!(matches!(
        &cards[0].id.source,
        ToastSource::Host { name } if *name == SHELL_WARNING_TOAST
    ));
}

#[test]
fn every_new_action_names_itself_in_the_transition_log() {
    assert_eq!(ShellIntent::ToggleNavPad.kind_name(), "toggle_nav_pad");
    assert_eq!(ShellIntent::CloseNavPad.kind_name(), "close_nav_pad");
    assert_eq!(
        ShellIntent::ShowWarning {
            message: String::new()
        }
        .kind_name(),
        "show_warning"
    );
}

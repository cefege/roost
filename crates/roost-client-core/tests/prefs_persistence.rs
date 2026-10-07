//! Preferences: they persist, they restore, and a corrupt one falls back instead
//! of stopping the boot.
//!
//! A preference is per-device state that outlives a reload, so the two halves are
//! one property: what a write stores is what the next boot reads. The third half
//! is the one that only shows up on someone else's machine — a value this build
//! did not write, a value out of the range this build allows, and a document that
//! is not what it claims to be. Every one of those resolves to the default, and
//! `load_prefs` has no failure path at all.
//!
//! The mutation experiment for this file is in the slice report: in
//! `read_flag`, treat an unrecognised stored value as `true` instead of falling
//! back to `default`, and
//! `a_corrupt_stored_preference_falls_back_instead_of_failing_the_boot` must fail.
use std::rc::Rc;

use roost_client_core::store::prefs::flags::{
    mouse_gestures_forwarded, set_copy_on_select, set_keyboard_resize, set_keyterm_biasing,
    set_mouse_forward, toggle_mouse_forward,
};
use roost_client_core::store::prefs::notify::{
    NotifyPref, confirm_desktop_notifications, set_notify_pref,
};
use roost_client_core::store::prefs::predict::{PredictMode, set_predict_mode};
use roost_client_core::store::prefs::terminal_font::{
    TERM_FONT_MAX_PX, TERM_FONT_MIN_PX, TERMINAL_FONT_DEFAULT_PX, TERMINAL_FONT_TV_DEFAULT_PX,
    reset_term_font_px, set_term_font_px, step_term_font_px,
};
use roost_client_core::store::prefs::terminal_ligatures::set_terminal_ligatures;
use roost_client_core::store::prefs::{
    COPY_ON_SELECT_KEY, KEYBOARD_RESIZE_KEY, KEYTERM_BIASING_KEY, MOUSE_FORWARD_KEY,
    NOTIFY_PREFS_KEY, PREDICT_MODE_KEY, PrefDefaults, TERM_FONT_PX_KEY, TERMINAL_LIGATURES_KEY,
    clear_account_scoped_prefs, load_prefs,
};
use roost_client_core::{ClientCore, KeyValueStore, MemoryClock, MemoryKeyValueStore};

fn client() -> ClientCore {
    ClientCore::in_memory("tab-prefs")
}

fn defaults() -> PrefDefaults {
    PrefDefaults::default()
}

/// A core over `storage`, so a test can boot a second one over the same bytes.
fn core_over(storage: &Rc<MemoryKeyValueStore>, defaults: &PrefDefaults) -> ClientCore {
    ClientCore::new(
        Rc::new(MemoryClock::new()),
        Rc::clone(storage) as Rc<dyn KeyValueStore>,
        "tab-prefs",
        defaults,
    )
}

#[test]
fn the_two_flags_that_default_off_load_as_off_and_the_two_that_default_on_load_as_on() {
    let mut core = client();
    let storage = MemoryKeyValueStore::default();
    assert!(
        !load_prefs(core.store_mut(), &storage, &defaults()),
        "a store that already holds the defaults must not repaint for them"
    );
    let store = core.store();
    assert!(
        !store.prefs.copy_on_select,
        "it silently overwrites the clipboard"
    );
    assert!(!store.prefs.keyboard_resize, "push is the calm default");
    assert!(
        store.prefs.keyterm_biasing,
        "the feature is on; the toggle A/Bs it"
    );
    assert!(
        store.prefs.mouse_forward,
        "an opt-in costs mouse-aware TUIs their mouse"
    );
    assert_eq!(store.prefs.term_font_px, 14);
    assert_eq!(store.prefs.predict, PredictMode::Adaptive);
    assert!(!store.prefs.terminal_ligatures);
    assert!(store.prefs.notify.in_app);
    assert!(
        !store.prefs.notify.desktop,
        "enabling it is a gesture, not a preference"
    );
    assert!(store.prefs.notify.title_badge);
    assert!(!store.prefs.notify.blocked_sound);
    assert!(!store.prefs.notify.done_sound);
}

#[test]
fn preferences_round_trip_through_storage() {
    let storage = MemoryKeyValueStore::default();
    {
        let mut core = client();
        let store = core.store_mut();
        assert!(set_copy_on_select(store, &storage, true));
        assert!(set_keyboard_resize(store, &storage, true));
        assert!(!set_keyterm_biasing(store, &storage, true), "already on");
        assert!(set_keyterm_biasing(store, &storage, false));
        assert!(toggle_mouse_forward(store, &storage));
        assert!(
            !mouse_gestures_forwarded(store, 0),
            "a zero tracking mode forwards nothing"
        );
        assert!(!mouse_gestures_forwarded(store, 2), "the device says no");
        assert!(set_mouse_forward(store, &storage, true));
        assert!(
            mouse_gestures_forwarded(store, 2),
            "the device says yes AND the app asked"
        );
        assert!(set_predict_mode(store, &storage, "force"));
        assert!(set_term_font_px(store, &storage, 19));
        assert!(step_term_font_px(store, &storage, 3));
        assert_eq!(store.prefs.term_font_px, 22);
        assert!(set_terminal_ligatures(store, &storage, true));
        assert!(
            !set_terminal_ligatures(store, &storage, true),
            "an unchanged preference is not a transition"
        );
        assert!(set_notify_pref(
            store,
            &storage,
            NotifyPref::BlockedSound,
            true
        ));
        assert!(confirm_desktop_notifications(store, &storage));
    }
    // Every key is written by the function that changed the value, so the next
    // boot has something to read.
    assert_eq!(storage.get(COPY_ON_SELECT_KEY).as_deref(), Some("1"));
    assert_eq!(storage.get(KEYBOARD_RESIZE_KEY).as_deref(), Some("1"));
    assert_eq!(storage.get(KEYTERM_BIASING_KEY).as_deref(), Some("0"));
    // `set_mouse_forward(store, &storage, true)` ran inside the block above
    // and was the LAST write to this key, so the stored flag is "1".
    // Asserting "0" asserted against the test's own sequence: the toggle's
    // "0" was overwritten before the block ended.
    assert_eq!(storage.get(MOUSE_FORWARD_KEY).as_deref(), Some("1"));
    assert_eq!(storage.get(PREDICT_MODE_KEY).as_deref(), Some("always"));
    assert_eq!(storage.get(TERM_FONT_PX_KEY).as_deref(), Some("22"));
    assert_eq!(storage.get(TERMINAL_LIGATURES_KEY).as_deref(), Some("1"));
    assert!(
        storage
            .get(NOTIFY_PREFS_KEY)
            .is_some_and(|raw| raw.contains("desktop"))
    );

    let mut rebooted = client();
    assert!(
        load_prefs(rebooted.store_mut(), &storage, &defaults()),
        "a boot that finds stored preferences repaints once"
    );
    let store = rebooted.store();
    assert!(store.prefs.copy_on_select);
    assert!(store.prefs.keyboard_resize);
    assert!(!store.prefs.keyterm_biasing);
    assert!(
        store.prefs.mouse_forward,
        "the stored \"1\" was this flag's LAST write, so the reboot reads it back on"
    );
    assert_eq!(store.prefs.predict, PredictMode::Always);
    assert_eq!(store.prefs.term_font_px, 22);
    assert!(store.prefs.terminal_ligatures);
    assert!(store.prefs.notify.desktop);
    assert!(store.prefs.notify.blocked_sound);
    assert!(
        store.prefs.notify.in_app,
        "an unstored member keeps its default"
    );
}

#[test]
fn a_corrupt_stored_preference_falls_back_instead_of_failing_the_boot() {
    let storage = MemoryKeyValueStore::default();
    // The one value this build CAN read, so the load has something to apply and
    // the corrupt ones are shown not to leak past it.
    storage.set(KEYBOARD_RESIZE_KEY, "1");
    // Four flavours of "a value this build did not write".
    storage.set(COPY_ON_SELECT_KEY, "yes");
    storage.set(KEYTERM_BIASING_KEY, "");
    storage.set(MOUSE_FORWARD_KEY, "{\"on\":true}");
    storage.set(TERM_FONT_PX_KEY, "-4");
    storage.set(PREDICT_MODE_KEY, "psychic");
    storage.set(NOTIFY_PREFS_KEY, "{not json at all");
    storage.set(TERMINAL_LIGATURES_KEY, "yes");
    let mut core = client();
    let before = core.store().revision();
    assert!(load_prefs(core.store_mut(), &storage, &defaults()));
    let store = core.store();
    assert_eq!(
        core.store().revision(),
        before + 1,
        "a load is one repaint however many values it had to replace"
    );
    assert!(
        store.prefs.keyboard_resize,
        "the one readable value applied"
    );
    assert!(!store.prefs.copy_on_select, "its default is off");
    assert!(
        store.prefs.keyterm_biasing,
        "its default is on — the default is a property of the flag"
    );
    assert!(store.prefs.mouse_forward, "its default is on");
    assert!(
        !store.prefs.terminal_ligatures,
        "an unrecognized ligature value falls back to off"
    );
    assert_eq!(
        store.prefs.term_font_px, 14,
        "an unparseable size is the device default"
    );
    assert_eq!(
        store.prefs.predict,
        PredictMode::Adaptive,
        "an unknown mode fails closed"
    );
    assert!(
        store.prefs.notify.in_app,
        "an unparseable document is the whole default"
    );
    assert!(!store.prefs.notify.desktop);
}

#[test]
fn the_two_spellings_an_earlier_build_wrote_still_mean_something() {
    let storage = MemoryKeyValueStore::default();
    storage.set(PREDICT_MODE_KEY, "0");
    let mut core = client();
    load_prefs(core.store_mut(), &storage, &defaults());
    assert_eq!(core.store().prefs.predict, PredictMode::Never);
    storage.set(PREDICT_MODE_KEY, "force");
    let mut other = client();
    load_prefs(other.store_mut(), &storage, &defaults());
    assert_eq!(other.store().prefs.predict, PredictMode::Always);
    // And writing normalises: the value on disk is one of the four.
    set_predict_mode(other.store_mut(), &storage, "force");
    assert_eq!(storage.get(PREDICT_MODE_KEY).as_deref(), Some("always"));
}

#[test]
fn the_terminal_font_size_is_bounded_at_both_ends_on_the_way_in_and_out() {
    let storage = MemoryKeyValueStore::default();
    let mut core = client();
    let store = core.store_mut();
    assert!(set_term_font_px(store, &storage, 0), "below the floor");
    assert_eq!(store.prefs.term_font_px, TERM_FONT_MIN_PX);
    assert!(
        set_term_font_px(store, &storage, 9_999),
        "above the ceiling"
    );
    assert_eq!(store.prefs.term_font_px, TERM_FONT_MAX_PX);
    // The stepper clamps at the same two ends as the setter, so a button cannot
    // walk a pane to 0 px or to 400 px.
    for _ in 0..40 {
        step_term_font_px(store, &storage, -1);
    }
    assert_eq!(store.prefs.term_font_px, TERM_FONT_MIN_PX);
    for _ in 0..80 {
        step_term_font_px(store, &storage, 1);
    }
    assert_eq!(store.prefs.term_font_px, TERM_FONT_MAX_PX);
    // A stored value from a build that shipped a different bound is bounded too.
    storage.set(TERM_FONT_PX_KEY, "4000");
    let mut rebooted = client();
    load_prefs(rebooted.store_mut(), &storage, &defaults());
    assert_eq!(rebooted.store().prefs.term_font_px, TERM_FONT_MAX_PX);
    storage.set(TERM_FONT_PX_KEY, "0");
    let mut floored = client();
    load_prefs(floored.store_mut(), &storage, &defaults());
    assert_eq!(floored.store().prefs.term_font_px, 14, "zero is not a size");
}

#[test]
fn a_television_gets_a_larger_default_and_only_on_first_run() {
    let storage = MemoryKeyValueStore::default();
    let tv = PrefDefaults {
        term_font_px: TERMINAL_FONT_TV_DEFAULT_PX,
    };
    let mut core = client();
    load_prefs(core.store_mut(), &storage, &tv);
    assert_eq!(core.store().prefs.term_font_px, TERMINAL_FONT_TV_DEFAULT_PX);
    assert!(set_term_font_px(core.store_mut(), &storage, 24));
    let mut rebooted = client();
    load_prefs(rebooted.store_mut(), &storage, &tv);
    assert_eq!(
        rebooted.store().prefs.term_font_px,
        24,
        "only the FIRST-RUN size moves; a stored user value still wins"
    );
    assert!(reset_term_font_px(
        rebooted.store_mut(),
        &storage,
        TERMINAL_FONT_TV_DEFAULT_PX
    ));
    assert_eq!(
        rebooted.store().prefs.term_font_px,
        TERMINAL_FONT_TV_DEFAULT_PX
    );
}

#[test]
fn a_notification_member_of_the_wrong_type_costs_only_itself() {
    let storage = MemoryKeyValueStore::default();
    storage.set(
        NOTIFY_PREFS_KEY,
        r#"{"inApp":"yes","titleBadge":false,"blockedSound":true,"futureMember":9}"#,
    );
    let mut core = client();
    load_prefs(core.store_mut(), &storage, &defaults());
    let notify = core.store().prefs.notify;
    assert!(
        notify.in_app,
        "a member of the wrong type keeps its default"
    );
    assert!(!notify.title_badge, "its neighbours still apply");
    assert!(notify.blocked_sound);
    assert!(
        !notify.desktop,
        "a member nobody wrote stays at its default"
    );
}

#[test]
fn notification_preferences_are_account_scoped_and_the_rest_are_device_scoped() {
    let storage = MemoryKeyValueStore::default();
    let mut core = client();
    let store = core.store_mut();
    set_notify_pref(store, &storage, NotifyPref::Desktop, true);
    set_term_font_px(store, &storage, 18);
    assert!(clear_account_scoped_prefs(store, &storage));
    assert!(
        !store.prefs.notify.desktop,
        "which machine to interrupt on is per account"
    );
    assert_eq!(
        store.prefs.term_font_px, 18,
        "how this browser behaves is per device"
    );
    assert!(storage.get(NOTIFY_PREFS_KEY).is_none());
    assert!(
        !clear_account_scoped_prefs(store, &storage),
        "already at the defaults"
    );
}

#[test]
fn a_new_core_over_stored_storage_starts_with_the_readers_preferences() {
    // The round trip above proves the WRITE stores what it should. This proves
    // the READ is on the boot path at all, which is a different thing: a host
    // that never calls `load_prefs` writes perfectly and restores nothing, and
    // every reader's zoom is gone by the next reload.
    let storage = Rc::new(MemoryKeyValueStore::default());
    let tv = PrefDefaults {
        term_font_px: TERMINAL_FONT_TV_DEFAULT_PX,
    };
    let writer: &dyn KeyValueStore = &*storage;
    let mut first = core_over(&storage, &tv);
    // Seeded through the real writer, so the key on disk is the one a product
    // would have written rather than a literal a test invented.
    assert!(set_term_font_px(first.store_mut(), writer, 19));
    assert!(set_copy_on_select(first.store_mut(), writer, true));

    let booted = core_over(&storage, &tv);
    assert_eq!(
        booted.store().prefs.term_font_px,
        19,
        "a boot that finds a stored zoom keeps it, so the first pane measures at it"
    );
    assert!(
        booted.store().prefs.copy_on_select,
        "the flags are on the same boot path as the zoom, and were not either"
    );
}

#[test]
fn a_first_run_with_nothing_stored_takes_the_device_default() {
    // The other half of the boot load: an absent key is not a reason to skip
    // it, and the device the host describes is the one that decides.
    let storage = Rc::new(MemoryKeyValueStore::default());
    let tv = PrefDefaults {
        term_font_px: TERMINAL_FONT_TV_DEFAULT_PX,
    };
    let booted = core_over(&storage, &tv);
    assert_eq!(
        booted.store().prefs.term_font_px,
        TERMINAL_FONT_TV_DEFAULT_PX
    );
    assert!(
        booted.store().prefs.mouse_forward,
        "absent means on, as it always has"
    );
    assert_ne!(
        TERMINAL_FONT_TV_DEFAULT_PX, TERMINAL_FONT_DEFAULT_PX,
        "the device default only differs from the desktop one if the constant moved"
    );
}

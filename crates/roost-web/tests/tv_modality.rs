//! TV mode's switch: the smart-TV user-agent list is the only automatic
//! signal (a pointerless viewport once switched real desktops into a D-pad UI
//! that suppresses PTY focus), `?tv=` wins over storage and persists because an
//! unpaired TV cannot reach Settings, and a stored value this build did not
//! write reads as `auto`. Pins `apps/web/src/lib/tvMode.ts`.

use roost_client_core::{KeyValueStore as _, MemoryKeyValueStore};
use roost_web::input_nav::modality::{
    ModeChoice, NavModality, TV_MODE_KEY, load_mode_choice, matches_tv_user_agent,
    pointerless_tv_viewport,
};

#[test]
fn smart_tv_user_agents_match() {
    for user_agent in [
        "Mozilla/5.0 (SMART-TV; Linux; Tizen 6.0) AppleWebKit/537.36 SamsungBrowser/4.0 TV Safari/537.36",
        "Mozilla/5.0 (Web0S; Linux/SmartTV) AppleWebKit/537.36 Chrome/79.0 Safari/537.36 WebAppManager",
        "Mozilla/5.0 (Linux; Android 9; BRAVIA 4K GB) AppleWebKit/537.36 Chrome/80.0 Safari/537.36",
        "Mozilla/5.0 (Linux; Android 12; Chromecast) AppleWebKit/537.36 (KHTML, like Gecko) CrKey/1.56.500000",
        "Mozilla/5.0 (Linux; Android TV 11) AppleWebKit/537.36 Chrome/96.0 Safari/537.36",
        "Roku/DVP-9.10 (519.10E04111A)",
    ] {
        assert!(matches_tv_user_agent(user_agent), "{user_agent}");
    }
}

#[test]
fn desktop_and_phone_user_agents_do_not_match() {
    for user_agent in [
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Safari/537.36",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 Version/17.6 Safari/605.1.15",
        "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148",
        // Word boundaries: a token inside a longer word is not a TV.
        "Mozilla/5.0 HDTVTuner/2.0 webosity/1 tizenish",
    ] {
        assert!(!matches_tv_user_agent(user_agent), "{user_agent}");
    }
}

#[test]
fn a_pointerless_viewport_is_never_a_tv_signal() {
    // Diagnostic only: NavModality never reads it, so a pointer-less desktop in
    // `auto` stays a desktop.
    assert!(pointerless_tv_viewport(true, 1920.0));
    let desktop = NavModality::new(ModeChoice::Auto, ModeChoice::Auto, false);
    assert!(!desktop.tv_mode_active());
    assert!(!desktop.directional_input_active());
}

#[test]
fn the_query_wins_and_is_persisted() {
    let storage = MemoryKeyValueStore::new();
    storage.set(TV_MODE_KEY, "off");

    assert_eq!(load_mode_choice(Some("1"), &storage, TV_MODE_KEY), ModeChoice::On);
    assert_eq!(storage.get(TV_MODE_KEY).as_deref(), Some("on"));
    // A later load with no query keeps the choice the one-time URL made.
    assert_eq!(load_mode_choice(None, &storage, TV_MODE_KEY), ModeChoice::On);

    assert_eq!(load_mode_choice(Some("false"), &storage, TV_MODE_KEY), ModeChoice::Off);
    assert_eq!(load_mode_choice(Some("auto"), &storage, TV_MODE_KEY), ModeChoice::Auto);
}

#[test]
fn an_unrecognised_query_or_stored_value_falls_back() {
    let storage = MemoryKeyValueStore::new();
    storage.set(TV_MODE_KEY, "off");
    assert_eq!(load_mode_choice(Some("yes"), &storage, TV_MODE_KEY), ModeChoice::Off);
    assert_eq!(storage.get(TV_MODE_KEY).as_deref(), Some("off"), "a bad query writes nothing");

    storage.set(TV_MODE_KEY, "1");
    assert_eq!(load_mode_choice(None, &storage, TV_MODE_KEY), ModeChoice::Auto);
}

#[test]
fn auto_follows_the_user_agent_and_the_choice_overrides_it() {
    let storage = MemoryKeyValueStore::new();
    let television = NavModality::load(None, None, &storage, true);
    assert!(television.tv_mode_active());
    assert_eq!(television.root_attributes()[0], ("data-tv", "true"));

    let mut forced_off = television;
    forced_off.set_tv_choice(&storage, ModeChoice::Off);
    assert!(!forced_off.tv_mode_active());
    assert_eq!(forced_off.root_attributes()[0], ("data-tv", "false"));
    assert_eq!(storage.get(TV_MODE_KEY).as_deref(), Some("off"));

    let forced_on = NavModality::load(Some("on"), None, &storage, false);
    assert!(forced_on.tv_mode_active());
}

//! The global keydown router's decisions: ⏎ never teleports the reader while a
//! terminal deck is on screen, a terminal keeps its body-focused Ctrl keys,
//! off-sidebar arrows stay native scroll, prevented keys are left alone, and
//! the platform chords (zoom, settings, palette) route. Ports
//! `apps/web/tests/keyboardShortcuts.test.ts` and
//! `apps/web/tests/browser/browserPlatform.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::keyboard_shortcuts::{KeydownAction, KeydownContext, keydown_action};
use roost_web::platform::browser_platform::{
    BrowserPlatform, NavigatorHints, PlatformShortcut, ShortcutKey, detect_browser_platform,
    is_alt_graph_event, matches_platform_shortcut, platform_shortcut_label,
};

fn key(value: &str) -> ShortcutKey {
    ShortcutKey {
        key: value.into(),
        ..ShortcutKey::default()
    }
}

fn ctrl(value: &str) -> ShortcutKey {
    ShortcutKey {
        ctrl: true,
        ..key(value)
    }
}

/// The sidebar's cursor parked on a second row, as v2's fixture does.
fn context(deck_present: bool) -> KeydownContext {
    KeydownContext {
        platform: BrowserPlatform::Linux,
        default_prevented: false,
        palette_open: false,
        help_open: false,
        controller_map_open: false,
        terminal_owns_keyboard: deck_present,
        target_in_terminal_input: false,
        focus_on_body: deck_present,
        target_is_text_field: false,
        target_editable: false,
        directional_input_active: false,
        cursor_row_selected: true,
        has_cursor_targets: true,
    }
}

#[test]
fn enter_never_activates_the_cursor_while_a_deck_is_mounted() {
    assert_eq!(
        keydown_action(&key("Enter"), &context(true)),
        KeydownAction::Ignore
    );
}

#[test]
fn enter_activates_the_cursor_in_a_pure_sidebar_view() {
    assert_eq!(
        keydown_action(&key("Enter"), &context(false)),
        KeydownAction::ActivateCursor
    );
}

#[test]
fn enter_never_activates_while_typing_in_an_input() {
    let typing = KeydownContext {
        target_editable: true,
        target_is_text_field: true,
        ..context(false)
    };
    assert_eq!(
        keydown_action(&key("Enter"), &typing),
        KeydownAction::Ignore
    );
}

#[test]
fn a_body_focused_terminal_keeps_ctrl_k() {
    let action = keydown_action(&ctrl("k"), &context(true));
    assert_eq!(action, KeydownAction::Ignore);
    assert!(!action.prevents_default());
}

#[test]
fn arrows_stay_native_scroll_when_no_cursor_rows_exist() {
    let empty = KeydownContext {
        has_cursor_targets: false,
        cursor_row_selected: false,
        ..context(false)
    };
    let action = keydown_action(&key("ArrowDown"), &empty);
    assert_eq!(action, KeydownAction::Ignore);
    assert!(!action.prevents_default());
    assert_eq!(
        keydown_action(&key("ArrowDown"), &context(false)),
        KeydownAction::MoveCursor(1)
    );
    assert_eq!(
        keydown_action(&key("ArrowUp"), &context(false)),
        KeydownAction::MoveCursor(-1)
    );
}

#[test]
fn enter_stays_a_focused_buttons_activation_until_a_row_is_highlighted() {
    let unselected = KeydownContext {
        cursor_row_selected: false,
        has_cursor_targets: false,
        ..context(false)
    };
    assert_eq!(
        keydown_action(&key("Enter"), &unselected),
        KeydownAction::Ignore
    );
}

#[test]
fn a_prevented_key_is_never_rerouted() {
    let prevented = KeydownContext {
        default_prevented: true,
        ..context(false)
    };
    for pressed in [
        ctrl("k"),
        ShortcutKey {
            shift: true,
            ..ctrl("u")
        },
        key("ArrowUp"),
        key("Enter"),
    ] {
        assert_eq!(keydown_action(&pressed, &prevented), KeydownAction::Ignore);
    }
}

#[test]
fn zoom_chords_route_only_while_a_terminal_owns_the_keyboard() {
    assert_eq!(
        keydown_action(&ctrl("="), &context(true)),
        KeydownAction::StepTermFont(1)
    );
    assert_eq!(
        keydown_action(&ctrl("-"), &context(true)),
        KeydownAction::StepTermFont(-1)
    );
    assert_eq!(
        keydown_action(&ctrl("0"), &context(true)),
        KeydownAction::ResetTermFont
    );
    assert_eq!(
        keydown_action(&ctrl("="), &context(false)),
        KeydownAction::Ignore
    );
}

#[test]
fn settings_and_the_palette_route_off_the_terminal() {
    assert_eq!(
        keydown_action(&ctrl(","), &context(false)),
        KeydownAction::OpenSettings
    );
    assert_eq!(
        keydown_action(&ctrl("k"), &context(false)),
        KeydownAction::TogglePalette
    );
    let palette = KeydownContext {
        palette_open: true,
        ..context(true)
    };
    assert_eq!(
        keydown_action(&key("Escape"), &palette),
        KeydownAction::ClosePalette
    );
    let shifted = ShortcutKey {
        shift: true,
        ..key("?")
    };
    assert_eq!(
        keydown_action(&shifted, &context(false)),
        KeydownAction::ToggleHelp
    );
}

#[test]
fn detection_prefers_client_hints_then_falls_back_to_the_user_agent() {
    let hints =
        |ua_data: Option<&str>, platform: Option<&str>, agent: Option<&str>| NavigatorHints {
            ua_data_platform: ua_data.map(str::to_owned),
            platform: platform.map(str::to_owned),
            user_agent: agent.map(str::to_owned),
        };
    assert_eq!(
        detect_browser_platform(&hints(Some("Windows"), None, None)),
        BrowserPlatform::Windows
    );
    assert_eq!(
        detect_browser_platform(&hints(None, Some("MacIntel"), None)),
        BrowserPlatform::MacOs
    );
    assert_eq!(
        detect_browser_platform(&hints(None, None, Some("Mozilla/5.0 (X11; Linux x86_64)"))),
        BrowserPlatform::Linux
    );
}

#[test]
fn mac_and_linux_labels_are_kept_and_windows_gets_its_own() {
    let label =
        |platform| platform_shortcut_label(PlatformShortcut::CommandPalette, "⌘K", platform);
    assert_eq!(label(BrowserPlatform::MacOs), "⌘K");
    assert_eq!(label(BrowserPlatform::Linux), "⌘K");
    assert_eq!(label(BrowserPlatform::Windows), "Ctrl+Shift+P");
}

#[test]
fn mac_and_linux_command_chords_match() {
    let meta_k = ShortcutKey {
        meta: true,
        ..key("k")
    };
    assert!(matches_platform_shortcut(
        &meta_k,
        PlatformShortcut::CommandPalette,
        BrowserPlatform::MacOs
    ));
    assert!(matches_platform_shortcut(
        &ctrl("k"),
        PlatformShortcut::CommandPalette,
        BrowserPlatform::Linux
    ));
    assert!(matches_platform_shortcut(
        &ctrl("b"),
        PlatformShortcut::ToggleSidebar,
        BrowserPlatform::Linux
    ));
}

#[test]
fn windows_never_binds_a_plain_ctrl_letter() {
    for (letter, shortcut) in [
        ("k", PlatformShortcut::CommandPalette),
        ("f", PlatformShortcut::SidebarSearch),
        ("b", PlatformShortcut::ToggleSidebar),
        ("t", PlatformShortcut::NewTerminal),
    ] {
        assert!(
            !matches_platform_shortcut(&ctrl(letter), shortcut, BrowserPlatform::Windows),
            "{letter}"
        );
    }
    let ctrl_shift = |letter: &str| ShortcutKey {
        shift: true,
        ..ctrl(letter)
    };
    assert!(matches_platform_shortcut(
        &ctrl_shift("p"),
        PlatformShortcut::CommandPalette,
        BrowserPlatform::Windows
    ));
    assert!(matches_platform_shortcut(
        &ctrl_shift("t"),
        PlatformShortcut::NewTerminal,
        BrowserPlatform::Windows
    ));
}

#[test]
fn alt_graph_never_triggers_an_application_shortcut() {
    let represented = ShortcutKey {
        alt: true,
        ..ctrl("t")
    };
    assert!(is_alt_graph_event(&represented, BrowserPlatform::Windows));
    assert!(!matches_platform_shortcut(
        &represented,
        PlatformShortcut::NewTerminal,
        BrowserPlatform::Windows
    ));
    let explicit = ShortcutKey {
        alt: true,
        alt_graph: true,
        ..ctrl("g")
    };
    assert!(is_alt_graph_event(&explicit, BrowserPlatform::Linux));
    assert!(!matches_platform_shortcut(
        &explicit,
        PlatformShortcut::ArrangeGrid,
        BrowserPlatform::Linux
    ));
}

#[test]
fn windows_pane_focus_and_tab_selection_use_alt_not_control() {
    let alt = |value: &str| ShortcutKey {
        alt: true,
        ..key(value)
    };
    assert!(matches_platform_shortcut(
        &alt("ArrowLeft"),
        PlatformShortcut::PaneFocus,
        BrowserPlatform::Windows
    ));
    assert!(matches_platform_shortcut(
        &alt("3"),
        PlatformShortcut::TerminalTab,
        BrowserPlatform::Windows
    ));
    let ctrl_alt = ShortcutKey {
        alt: true,
        ..ctrl("ArrowLeft")
    };
    assert!(!matches_platform_shortcut(
        &ctrl_alt,
        PlatformShortcut::PaneFocus,
        BrowserPlatform::Windows
    ));
}

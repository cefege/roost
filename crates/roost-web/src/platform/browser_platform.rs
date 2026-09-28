//! Browser OS detection and the one application-shortcut map. Terminal-facing
//! code asks this module rather than treating Ctrl as Command: on Windows a plain
//! Ctrl+letter belongs to the PTY and Ctrl+Alt may be AltGraph. Ports
//! `apps/web/src/browser/browserPlatform.ts`; read by `keyboard_shortcuts`, the
//! shell's sidebar toggle, the deck's pane chords and the terminal's link key.
//!
//! Pure: a key event is the plain [`ShortcutKey`] record, not a DOM event.

/// The OS family a browser runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPlatform {
    /// macOS or iOS.
    MacOs,
    /// Windows.
    Windows,
    /// Linux, ChromeOS or Android.
    Linux,
    /// Anything else.
    Other,
}

/// The navigator fields detection reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigatorHints {
    /// `navigator.userAgent`.
    pub user_agent: Option<String>,
    /// `navigator.platform`.
    pub platform: Option<String>,
    /// `navigator.userAgentData.platform` (UA client hints), which wins.
    pub ua_data_platform: Option<String>,
}

/// A key press, as the shortcut matcher reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutKey {
    /// `KeyboardEvent.key`.
    pub key: String,
    /// Control held.
    pub ctrl: bool,
    /// Alt/Option held.
    pub alt: bool,
    /// Shift held.
    pub shift: bool,
    /// Meta/Command held.
    pub meta: bool,
    /// `getModifierState("AltGraph")`.
    pub alt_graph: bool,
}

/// Every application shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformShortcut {
    CommandPalette,
    SidebarSearch,
    ToggleSidebar,
    Settings,
    TermFontIncrease,
    TermFontDecrease,
    TermFontReset,
    TerminalTab,
    PaneFocus,
    NewTerminal,
    SplitRight,
    SplitDown,
    Spotlight,
    ArrangeBalance,
    ArrangeGrid,
    ArrangeColumns,
    ArrangeRows,
    ArrangeMain,
    TerminalCopy,
    TerminalPaste,
    TerminalFind,
}

impl PlatformShortcut {
    /// The Windows binding's label, which replaces the macOS/Linux one.
    pub const fn windows_label(self) -> &'static str {
        match self {
            Self::CommandPalette => "Ctrl+Shift+P",
            Self::SidebarSearch | Self::TerminalFind => "Ctrl+Shift+F",
            Self::ToggleSidebar => "Ctrl+Shift+B",
            Self::Settings => "Ctrl+,",
            Self::TermFontIncrease => "Ctrl++",
            Self::TermFontDecrease => "Ctrl+−",
            Self::TermFontReset => "Ctrl+0",
            Self::TerminalTab => "Alt+1–9",
            Self::PaneFocus => "Alt+← ↑ → ↓",
            Self::NewTerminal => "Ctrl+Shift+T",
            Self::SplitRight => "Alt+Shift+D",
            Self::SplitDown => "Alt+Shift+S",
            Self::Spotlight => "Alt+Enter",
            Self::ArrangeBalance => "Alt+Shift+B",
            Self::ArrangeGrid => "Alt+Shift+G",
            Self::ArrangeColumns => "Alt+Shift+E",
            Self::ArrangeRows => "Alt+Shift+R",
            Self::ArrangeMain => "Alt+Shift+V",
            Self::TerminalCopy => "Ctrl+Shift+C",
            Self::TerminalPaste => "Ctrl+Shift+V",
        }
    }
}

/// Detect the platform: client hints first, then `platform`, then the UA.
pub fn detect_browser_platform(hints: &NavigatorHints) -> BrowserPlatform {
    let hinted = hints
        .ua_data_platform
        .as_deref()
        .or(hints.platform.as_deref())
        .unwrap_or("");
    let haystack = format!("{hinted} {}", hints.user_agent.as_deref().unwrap_or("")).to_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|needle| haystack.contains(needle));
    if any(&["windows", "win32", "win64"]) {
        BrowserPlatform::Windows
    } else if any(&["macos", "macintosh", "macintel", "iphone", "ipad"]) {
        BrowserPlatform::MacOs
    } else if any(&["linux", "x11", "cros", "android"]) {
        BrowserPlatform::Linux
    } else {
        BrowserPlatform::Other
    }
}

/// Whether this press is AltGraph — never an application shortcut, on Windows
/// including its Ctrl+Alt representation.
pub fn is_alt_graph_event(event: &ShortcutKey, platform: BrowserPlatform) -> bool {
    event.key == "AltGraph"
        || event.alt_graph
        || (platform == BrowserPlatform::Windows && event.ctrl && event.alt && !event.meta)
}

/// Whether `event` is `shortcut` on `platform`.
pub fn matches_platform_shortcut(
    event: &ShortcutKey,
    shortcut: PlatformShortcut,
    platform: BrowserPlatform,
) -> bool {
    if is_alt_graph_event(event, platform) {
        return false;
    }
    let key = lower_key(&event.key);
    if platform == BrowserPlatform::Windows {
        matches_windows(event, &key, shortcut)
    } else {
        matches_mac_linux(event, &key, shortcut)
    }
}

/// The label to show: the caller's macOS/Linux label, or the Windows binding.
pub fn platform_shortcut_label(
    shortcut: PlatformShortcut,
    mac_linux_label: &str,
    platform: BrowserPlatform,
) -> &str {
    if platform == BrowserPlatform::Windows {
        shortcut.windows_label()
    } else {
        mac_linux_label
    }
}

/// The modifier that makes a terminal link clickable.
pub fn terminal_link_modifier_key(platform: BrowserPlatform) -> &'static str {
    if platform == BrowserPlatform::MacOs {
        "Meta"
    } else {
        "Control"
    }
}

fn lower_key(key: &str) -> String {
    if key.chars().count() == 1 {
        key.to_lowercase()
    } else {
        key.to_owned()
    }
}

fn is_digit_1_9(key: &str) -> bool {
    matches!(key.as_bytes(), [b'1'..=b'9'])
}

fn is_arrow(key: &str) -> bool {
    matches!(key, "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown")
}

fn matches_windows(event: &ShortcutKey, key: &str, shortcut: PlatformShortcut) -> bool {
    use PlatformShortcut as S;
    let ctrl_shift =
        |letter: &str| key == letter && event.ctrl && event.shift && !event.alt && !event.meta;
    let alt_shift =
        |letter: &str| key == letter && event.alt && event.shift && !event.ctrl && !event.meta;
    let alt_only = event.alt && !event.shift && !event.ctrl && !event.meta;
    let ctrl_no_alt_meta = event.ctrl && !event.alt && !event.meta;
    match shortcut {
        S::CommandPalette => ctrl_shift("p"),
        S::SidebarSearch | S::TerminalFind => ctrl_shift("f"),
        S::ToggleSidebar => ctrl_shift("b"),
        S::Settings => key == "," && event.ctrl && !event.shift && !event.alt && !event.meta,
        S::TermFontIncrease => matches!(key, "+" | "=") && ctrl_no_alt_meta,
        S::TermFontDecrease => matches!(key, "-" | "_") && ctrl_no_alt_meta,
        S::TermFontReset => key == "0" && ctrl_no_alt_meta,
        S::TerminalTab => is_digit_1_9(key) && alt_only,
        S::PaneFocus => is_arrow(key) && alt_only,
        S::NewTerminal => ctrl_shift("t"),
        S::SplitRight => alt_shift("d"),
        S::SplitDown => alt_shift("s"),
        S::Spotlight => key == "Enter" && alt_only,
        S::ArrangeBalance => alt_shift("b"),
        S::ArrangeGrid => alt_shift("g"),
        S::ArrangeColumns => alt_shift("e"),
        S::ArrangeRows => alt_shift("r"),
        S::ArrangeMain => alt_shift("v"),
        S::TerminalCopy => ctrl_shift("c"),
        S::TerminalPaste => ctrl_shift("v"),
    }
}

fn matches_mac_linux(event: &ShortcutKey, key: &str, shortcut: PlatformShortcut) -> bool {
    use PlatformShortcut as S;
    let primary = event.meta || event.ctrl;
    let meta_only = event.meta && !event.ctrl;
    let meta_alt = |letter: &str| meta_only && event.alt && key == letter;
    match shortcut {
        S::CommandPalette => primary && key == "k",
        S::SidebarSearch => primary && key == "f",
        S::ToggleSidebar => primary && key == "b" && !event.shift,
        S::Settings => primary && key == "," && !event.alt,
        S::TermFontIncrease => primary && !event.alt && matches!(key, "+" | "="),
        S::TermFontDecrease => primary && !event.alt && matches!(key, "-" | "_"),
        S::TermFontReset => primary && !event.alt && key == "0",
        S::TerminalTab => primary && !event.alt && is_digit_1_9(key),
        S::PaneFocus => primary && event.alt && is_arrow(key),
        S::NewTerminal => primary && event.alt && key == "t",
        S::SplitRight => meta_only && !event.alt && !event.shift && key == "d",
        S::SplitDown => meta_only && !event.alt && event.shift && key == "d",
        S::Spotlight => meta_only && !event.alt && key == "Enter",
        S::ArrangeBalance => meta_alt("b"),
        S::ArrangeGrid => meta_alt("g"),
        S::ArrangeColumns => meta_alt("e"),
        S::ArrangeRows => meta_alt("r"),
        S::ArrangeMain => meta_alt("v"),
        S::TerminalCopy => primary && event.shift && !event.alt && key == "c",
        S::TerminalPaste => primary && event.shift && !event.alt && key == "v",
        S::TerminalFind => {
            !event.alt && key == "f" && ((meta_only && !event.shift) || (event.ctrl && event.shift))
        }
    }
}

/// This browser's platform.
#[cfg(target_arch = "wasm32")]
pub fn browser_platform() -> BrowserPlatform {
    let Some(window) = web_sys::window() else {
        return BrowserPlatform::Other;
    };
    let navigator = window.navigator();
    let ua_data_platform = js_sys::Reflect::get(&navigator, &"userAgentData".into())
        .ok()
        .filter(|data| data.is_object())
        .and_then(|data| js_sys::Reflect::get(&data, &"platform".into()).ok())
        .and_then(|platform| platform.as_string());
    detect_browser_platform(&NavigatorHints {
        user_agent: navigator.user_agent().ok(),
        platform: navigator.platform().ok(),
        ua_data_platform,
    })
}

/// A key event as the matcher reads it.
#[cfg(target_arch = "wasm32")]
pub fn shortcut_key(event: &web_sys::KeyboardEvent) -> ShortcutKey {
    ShortcutKey {
        key: event.key(),
        ctrl: event.ctrl_key(),
        alt: event.alt_key(),
        shift: event.shift_key(),
        meta: event.meta_key(),
        alt_graph: event.get_modifier_state("AltGraph"),
    }
}

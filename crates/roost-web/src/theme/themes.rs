//! The theme registry: the Light and Dark palettes, the ids `auto` resolves to,
//! and the default. Ported from `apps/web/src/lib/themes.ts`; read by
//! `choice.rs` (resolution and application) and by the Settings theme picker.
//!
//! Surface, status, syntax and ANSI roles stay stable across themes while the
//! `wb-*` roles mirror the VS Code 1.137.0 Dark Modern and Light Modern chrome.
//! This file is where raw colours are DECLARED — the design raw-value ratchet
//! (`xtask/src/design_raw.rs`) exempts `themes.rs` for that reason — and every
//! other file reaches them through `var(--…)`.

use super::tokens::{CanonicalToken, Theme, ThemeAppearance, ThemeGroup};

/// The id `auto` resolves to when the OS prefers dark.
pub const SYSTEM_DARK_ID: &str = DARK.id;

/// The id `auto` resolves to when the OS prefers light.
pub const SYSTEM_LIGHT_ID: &str = LIGHT.id;

/// The id an unknown choice, or an unreadable OS preference, resolves to.
pub const DEFAULT_THEME_ID: &str = DARK.id;

/// Every registered theme, in picker order.
pub static THEMES: [Theme; 2] = [LIGHT, DARK];

/// The theme registered under an id.
pub fn theme_by_id(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

/// The default theme: the one `DEFAULT_THEME_ID` names.
pub fn default_theme() -> &'static Theme {
    &DARK
}

/// Dark: restrained neutral chrome; terminal, status and syntax roles stable.
const DARK: Theme = Theme {
    id: "graphite",
    label: "Dark",
    group: ThemeGroup::Dark,
    appearance: ThemeAppearance::Dark,
    palette: dark_palette,
};

/// Light: restrained neutral chrome; terminal, status and syntax roles stable.
const LIGHT: Theme = Theme {
    id: "light",
    label: "Light",
    group: ThemeGroup::Light,
    appearance: ThemeAppearance::Light,
    palette: light_palette,
};

fn dark_palette(token: CanonicalToken) -> &'static str {
    match token {
        CanonicalToken::BgBase => "#0a0a0a",
        CanonicalToken::Surface0 => "#0a0a0a",
        CanonicalToken::Surface1 => "#171717",
        CanonicalToken::Surface2 => "#262626",
        CanonicalToken::Surface3 => "#404040",
        CanonicalToken::TermBg => "#0a0f11",
        CanonicalToken::TextHi => "#fafafa",
        CanonicalToken::TextMid => "#d4d4d4",
        CanonicalToken::TextLo => "#a3a3a3",
        CanonicalToken::TermFg => "#e3e3e1",
        CanonicalToken::Accent => "#fafafa",
        CanonicalToken::OnAccent => "#171717",
        CanonicalToken::AccentContainer => "#262626",
        CanonicalToken::OnAccentContainer => "#fafafa",
        CanonicalToken::BorderStrong => "#525252",
        CanonicalToken::BorderSubtle => "#262626",
        CanonicalToken::StatusOk => "#81c995",
        CanonicalToken::StatusWarn => "#fdd663",
        CanonicalToken::StatusErr => "#f28b82",
        CanonicalToken::StatusInfo => "#7fd1ec",
        CanonicalToken::SyntaxPlain => "#e3e3e1",
        CanonicalToken::SyntaxKeyword => "#7fd1ec",
        CanonicalToken::SyntaxString => "#81c995",
        CanonicalToken::SyntaxNumber => "#fdd663",
        CanonicalToken::SyntaxComment => "#8c969b",
        CanonicalToken::SecondaryContainer => "#262626",
        CanonicalToken::OnSecondaryContainer => "#fafafa",
        CanonicalToken::WbTitlebar => "#171717",
        CanonicalToken::WbActivity => "#171717",
        CanonicalToken::WbSidebar => "#171717",
        CanonicalToken::WbEditor => "#0a0a0a",
        CanonicalToken::WbStatusbar => "#171717",
        CanonicalToken::WbBorder => "#262626",
        CanonicalToken::WbActive => "#fafafa",
        CanonicalToken::WbActiveContrast => "#0a0a0a",
        CanonicalToken::WbFocus => "#fafafa",
        CanonicalToken::WbSelected => "#262626",
        CanonicalToken::WbSelectedContrast => "#fafafa",
        CanonicalToken::WbTabActive => "#0a0a0a",
        CanonicalToken::WbTabInactive => "#171717",
        CanonicalToken::WbTabHover => "#262626",
        CanonicalToken::WbPaneDrop => "#fafafa26",
        CanonicalToken::WbPaneFocus => "#fafafa",
        CanonicalToken::AnsiBlack => "#242d33",
        CanonicalToken::AnsiRed => "#f28b82",
        CanonicalToken::AnsiGreen => "#81c995",
        CanonicalToken::AnsiYellow => "#fdd663",
        CanonicalToken::AnsiBlue => "#7fd1ec",
        CanonicalToken::AnsiMagenta => "#c58af9",
        CanonicalToken::AnsiCyan => "#78d9ec",
        CanonicalToken::AnsiWhite => "#e3e3e1",
        CanonicalToken::AnsiBrightBlack => "#5a646a",
        CanonicalToken::AnsiBrightRed => "#f6aea9",
        CanonicalToken::AnsiBrightGreen => "#a8dab5",
        CanonicalToken::AnsiBrightYellow => "#fde293",
        CanonicalToken::AnsiBrightBlue => "#bde9ff",
        CanonicalToken::AnsiBrightMagenta => "#d7aefb",
        CanonicalToken::AnsiBrightCyan => "#a1e4f2",
        CanonicalToken::AnsiBrightWhite => "#ffffff",
    }
}

fn light_palette(token: CanonicalToken) -> &'static str {
    match token {
        CanonicalToken::BgBase => "#ffffff",
        CanonicalToken::Surface0 => "#ffffff",
        CanonicalToken::Surface1 => "#fafafa",
        CanonicalToken::Surface2 => "#f5f5f5",
        CanonicalToken::Surface3 => "#e5e5e5",
        CanonicalToken::TermBg => "#ffffff",
        CanonicalToken::TextHi => "#171717",
        CanonicalToken::TextMid => "#525252",
        CanonicalToken::TextLo => "#737373",
        CanonicalToken::TermFg => "#1f1f1f",
        CanonicalToken::Accent => "#171717",
        CanonicalToken::OnAccent => "#fafafa",
        CanonicalToken::AccentContainer => "#f5f5f5",
        CanonicalToken::OnAccentContainer => "#171717",
        CanonicalToken::BorderStrong => "#d4d4d4",
        CanonicalToken::BorderSubtle => "#e5e5e5",
        CanonicalToken::StatusOk => "#188038",
        CanonicalToken::StatusWarn => "#b06000",
        CanonicalToken::StatusErr => "#c5221f",
        CanonicalToken::StatusInfo => "#0b57d0",
        CanonicalToken::SyntaxPlain => "#383a42",
        CanonicalToken::SyntaxKeyword => "#a626a4",
        CanonicalToken::SyntaxString => "#50a14f",
        CanonicalToken::SyntaxNumber => "#c18401",
        CanonicalToken::SyntaxComment => "#a0a1a7",
        CanonicalToken::SecondaryContainer => "#f5f5f5",
        CanonicalToken::OnSecondaryContainer => "#171717",
        CanonicalToken::WbTitlebar => "#fafafa",
        CanonicalToken::WbActivity => "#fafafa",
        CanonicalToken::WbSidebar => "#fafafa",
        CanonicalToken::WbEditor => "#ffffff",
        CanonicalToken::WbStatusbar => "#fafafa",
        CanonicalToken::WbBorder => "#e5e5e5",
        CanonicalToken::WbActive => "#171717",
        CanonicalToken::WbActiveContrast => "#ffffff",
        CanonicalToken::WbFocus => "#171717",
        CanonicalToken::WbSelected => "#f5f5f5",
        CanonicalToken::WbSelectedContrast => "#171717",
        CanonicalToken::WbTabActive => "#ffffff",
        CanonicalToken::WbTabInactive => "#fafafa",
        CanonicalToken::WbTabHover => "#f5f5f5",
        CanonicalToken::WbPaneDrop => "#17171726",
        CanonicalToken::WbPaneFocus => "#171717",
        CanonicalToken::AnsiBlack => "#383a42",
        CanonicalToken::AnsiRed => "#e45649",
        CanonicalToken::AnsiGreen => "#50a14f",
        CanonicalToken::AnsiYellow => "#c18401",
        CanonicalToken::AnsiBlue => "#4078f2",
        CanonicalToken::AnsiMagenta => "#a626a4",
        CanonicalToken::AnsiCyan => "#0184bc",
        CanonicalToken::AnsiWhite => "#a0a1a7",
        CanonicalToken::AnsiBrightBlack => "#696c77",
        CanonicalToken::AnsiBrightRed => "#e45649",
        CanonicalToken::AnsiBrightGreen => "#50a14f",
        CanonicalToken::AnsiBrightYellow => "#986801",
        CanonicalToken::AnsiBrightBlue => "#4078f2",
        CanonicalToken::AnsiBrightMagenta => "#a626a4",
        CanonicalToken::AnsiBrightCyan => "#0184bc",
        CanonicalToken::AnsiBrightWhite => "#ffffff",
    }
}

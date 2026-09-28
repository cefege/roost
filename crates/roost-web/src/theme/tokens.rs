//! The canonical theme token contract: the ONE list of colour roles a theme
//! must define, and the theme record that defines them. Ported from
//! `apps/web/src/lib/themeTokens.ts`; `themes.rs` supplies the palettes and
//! `choice.rs` turns a palette into what the document is given.
//!
//! Every other colour token in the app (`--md-*`, `--md-sys-color-*`, `--bg-*`,
//! the status and terminal roles) is a static `var(--<canonical>)` alias in
//! `assets/styles/theme-vars.css`, so a theme supplies only this set and the
//! whole alias graph reflows. A palette is an exhaustive `match` over
//! [`CanonicalToken`], so a theme that omits a role does not compile — the
//! Rust form of v2's `Record<CanonicalToken, string>`.

/// A canonical colour role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CanonicalToken {
    // Surfaces (low → high elevation) and the terminal's surrounding background.
    BgBase,
    Surface0,
    Surface1,
    Surface2,
    Surface3,
    TermBg,
    // Text (high → low emphasis) and the terminal foreground.
    TextHi,
    TextMid,
    TextLo,
    TermFg,
    // Accent / primary.
    Accent,
    OnAccent,
    AccentContainer,
    OnAccentContainer,
    // Outlines.
    BorderStrong,
    BorderSubtle,
    // Semantic status.
    StatusOk,
    StatusWarn,
    StatusErr,
    StatusInfo,
    // Syntax highlighting (code blocks, file viewer).
    SyntaxPlain,
    SyntaxKeyword,
    SyntaxString,
    SyntaxNumber,
    SyntaxComment,
    // Selection / active tint: every selected state (nav rail, list row, theme tile).
    SecondaryContainer,
    OnSecondaryContainer,
    // Workbench chrome roles; the `--workbench-*` aliases in `theme-vars.css` read these.
    WbTitlebar,
    WbActivity,
    WbSidebar,
    WbEditor,
    WbStatusbar,
    WbBorder,
    WbActive,
    WbActiveContrast,
    WbFocus,
    WbSelected,
    WbSelectedContrast,
    WbTabActive,
    WbTabInactive,
    WbTabHover,
    WbPaneDrop,
    WbPaneFocus,
    // ANSI 16. Cell spans read these through the `--term-color-N` bridge in `sidebar.css`.
    AnsiBlack,
    AnsiRed,
    AnsiGreen,
    AnsiYellow,
    AnsiBlue,
    AnsiMagenta,
    AnsiCyan,
    AnsiWhite,
    AnsiBrightBlack,
    AnsiBrightRed,
    AnsiBrightGreen,
    AnsiBrightYellow,
    AnsiBrightBlue,
    AnsiBrightMagenta,
    AnsiBrightCyan,
    AnsiBrightWhite,
}

impl CanonicalToken {
    /// Every role, in v2's declaration order.
    pub const ALL: [Self; 59] = [
        Self::BgBase,
        Self::Surface0,
        Self::Surface1,
        Self::Surface2,
        Self::Surface3,
        Self::TermBg,
        Self::TextHi,
        Self::TextMid,
        Self::TextLo,
        Self::TermFg,
        Self::Accent,
        Self::OnAccent,
        Self::AccentContainer,
        Self::OnAccentContainer,
        Self::BorderStrong,
        Self::BorderSubtle,
        Self::StatusOk,
        Self::StatusWarn,
        Self::StatusErr,
        Self::StatusInfo,
        Self::SyntaxPlain,
        Self::SyntaxKeyword,
        Self::SyntaxString,
        Self::SyntaxNumber,
        Self::SyntaxComment,
        Self::SecondaryContainer,
        Self::OnSecondaryContainer,
        Self::WbTitlebar,
        Self::WbActivity,
        Self::WbSidebar,
        Self::WbEditor,
        Self::WbStatusbar,
        Self::WbBorder,
        Self::WbActive,
        Self::WbActiveContrast,
        Self::WbFocus,
        Self::WbSelected,
        Self::WbSelectedContrast,
        Self::WbTabActive,
        Self::WbTabInactive,
        Self::WbTabHover,
        Self::WbPaneDrop,
        Self::WbPaneFocus,
        Self::AnsiBlack,
        Self::AnsiRed,
        Self::AnsiGreen,
        Self::AnsiYellow,
        Self::AnsiBlue,
        Self::AnsiMagenta,
        Self::AnsiCyan,
        Self::AnsiWhite,
        Self::AnsiBrightBlack,
        Self::AnsiBrightRed,
        Self::AnsiBrightGreen,
        Self::AnsiBrightYellow,
        Self::AnsiBrightBlue,
        Self::AnsiBrightMagenta,
        Self::AnsiBrightCyan,
        Self::AnsiBrightWhite,
    ];

    /// The role's name: the custom property without its `--`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BgBase => "bg-base",
            Self::Surface0 => "surface-0",
            Self::Surface1 => "surface-1",
            Self::Surface2 => "surface-2",
            Self::Surface3 => "surface-3",
            Self::TermBg => "term-bg",
            Self::TextHi => "text-hi",
            Self::TextMid => "text-mid",
            Self::TextLo => "text-lo",
            Self::TermFg => "term-fg",
            Self::Accent => "accent",
            Self::OnAccent => "on-accent",
            Self::AccentContainer => "accent-container",
            Self::OnAccentContainer => "on-accent-container",
            Self::BorderStrong => "border-strong",
            Self::BorderSubtle => "border-subtle",
            Self::StatusOk => "status-ok",
            Self::StatusWarn => "status-warn",
            Self::StatusErr => "status-err",
            Self::StatusInfo => "status-info",
            Self::SyntaxPlain => "syntax-plain",
            Self::SyntaxKeyword => "syntax-keyword",
            Self::SyntaxString => "syntax-string",
            Self::SyntaxNumber => "syntax-number",
            Self::SyntaxComment => "syntax-comment",
            Self::SecondaryContainer => "secondary-container",
            Self::OnSecondaryContainer => "on-secondary-container",
            Self::WbTitlebar => "wb-titlebar",
            Self::WbActivity => "wb-activity",
            Self::WbSidebar => "wb-sidebar",
            Self::WbEditor => "wb-editor",
            Self::WbStatusbar => "wb-statusbar",
            Self::WbBorder => "wb-border",
            Self::WbActive => "wb-active",
            Self::WbActiveContrast => "wb-active-contrast",
            Self::WbFocus => "wb-focus",
            Self::WbSelected => "wb-selected",
            Self::WbSelectedContrast => "wb-selected-contrast",
            Self::WbTabActive => "wb-tab-active",
            Self::WbTabInactive => "wb-tab-inactive",
            Self::WbTabHover => "wb-tab-hover",
            Self::WbPaneDrop => "wb-pane-drop",
            Self::WbPaneFocus => "wb-pane-focus",
            Self::AnsiBlack => "ansi-black",
            Self::AnsiRed => "ansi-red",
            Self::AnsiGreen => "ansi-green",
            Self::AnsiYellow => "ansi-yellow",
            Self::AnsiBlue => "ansi-blue",
            Self::AnsiMagenta => "ansi-magenta",
            Self::AnsiCyan => "ansi-cyan",
            Self::AnsiWhite => "ansi-white",
            Self::AnsiBrightBlack => "ansi-bright-black",
            Self::AnsiBrightRed => "ansi-bright-red",
            Self::AnsiBrightGreen => "ansi-bright-green",
            Self::AnsiBrightYellow => "ansi-bright-yellow",
            Self::AnsiBrightBlue => "ansi-bright-blue",
            Self::AnsiBrightMagenta => "ansi-bright-magenta",
            Self::AnsiBrightCyan => "ansi-bright-cyan",
            Self::AnsiBrightWhite => "ansi-bright-white",
        }
    }

    /// The custom property the role is written to, `--<name>`.
    pub fn custom_property(self) -> String {
        format!("--{}", self.name())
    }
}

/// Whether a theme is light or dark; written to `color-scheme` so native form
/// controls and scrollbars match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeAppearance {
    /// A light theme.
    Light,
    /// A dark theme.
    Dark,
}

impl ThemeAppearance {
    /// The `color-scheme` value.
    pub const fn color_scheme(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// The picker group a theme is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeGroup {
    /// The synthetic "follow the OS" entry.
    System,
    /// Light themes.
    Light,
    /// Dark themes.
    Dark,
    /// Palette themes.
    Palette,
}

impl ThemeGroup {
    /// The picker's group order.
    pub const ORDER: [Self; 4] = [Self::System, Self::Light, Self::Dark, Self::Palette];

    /// The group's heading.
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::Palette => "Palette",
        }
    }
}

/// One registered theme.
///
/// No `PartialEq`: two themes are the same theme when their ids are, and a
/// derived comparison would compare palette function addresses instead.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// The id persisted and written to `data-theme`.
    pub id: &'static str,
    /// The picker label.
    pub label: &'static str,
    /// The picker group.
    pub group: ThemeGroup,
    /// Light or dark.
    pub appearance: ThemeAppearance,
    /// The complete canonical palette.
    pub palette: fn(CanonicalToken) -> &'static str,
}

impl Theme {
    /// The colour this theme gives a role.
    pub fn token(&self, token: CanonicalToken) -> &'static str {
        (self.palette)(token)
    }
}

//! ANSI colors remain data-driven terminal colors. Terminal surfaces and plain
//! text follow rust-ui's document roles; syntax accents remain editor data.

/// Roost-owned ANSI and syntax roles; rust-ui owns the terminal surface roles.
/// Each appearance has one set and the base/accent palettes never touch them.
/// The dark terminal and syntax roles.
pub const ROOST_DARK: &[(&str, &str)] = &[
    ("--term-bg", "var(--background)"),
    ("--term-fg", "var(--foreground)"),
    ("--syntax-plain", "var(--foreground)"),
    ("--syntax-keyword", "#7fd1ec"),
    ("--syntax-string", "#81c995"),
    ("--syntax-number", "#fdd663"),
    ("--syntax-comment", "#8c969b"),
    ("--ansi-black", "#242d33"),
    ("--ansi-red", "#f28b82"),
    ("--ansi-green", "#81c995"),
    ("--ansi-yellow", "#fdd663"),
    ("--ansi-blue", "#7fd1ec"),
    ("--ansi-magenta", "#c58af9"),
    ("--ansi-cyan", "#78d9ec"),
    ("--ansi-white", "#e3e3e1"),
    ("--ansi-bright-black", "#5a646a"),
    ("--ansi-bright-red", "#f6aea9"),
    ("--ansi-bright-green", "#a8dab5"),
    ("--ansi-bright-yellow", "#fde293"),
    ("--ansi-bright-blue", "#bde9ff"),
    ("--ansi-bright-magenta", "#d7aefb"),
    ("--ansi-bright-cyan", "#a1e4f2"),
    ("--ansi-bright-white", "#ffffff"),
];

/// The light ANSI and syntax roles.
pub const ROOST_LIGHT: &[(&str, &str)] = &[
    ("--term-bg", "var(--background)"),
    ("--term-fg", "var(--foreground)"),
    ("--syntax-plain", "var(--foreground)"),
    ("--syntax-keyword", "#a626a4"),
    ("--syntax-string", "#50a14f"),
    ("--syntax-number", "#c18401"),
    ("--syntax-comment", "#a0a1a7"),
    ("--ansi-black", "#383a42"),
    ("--ansi-red", "#e45649"),
    ("--ansi-green", "#50a14f"),
    ("--ansi-yellow", "#c18401"),
    ("--ansi-blue", "#4078f2"),
    ("--ansi-magenta", "#a626a4"),
    ("--ansi-cyan", "#0184bc"),
    ("--ansi-white", "#a0a1a7"),
    ("--ansi-bright-black", "#696c77"),
    ("--ansi-bright-red", "#e45649"),
    ("--ansi-bright-green", "#50a14f"),
    ("--ansi-bright-yellow", "#986801"),
    ("--ansi-bright-blue", "#4078f2"),
    ("--ansi-bright-magenta", "#a626a4"),
    ("--ansi-bright-cyan", "#0184bc"),
    ("--ansi-bright-white", "#ffffff"),
];

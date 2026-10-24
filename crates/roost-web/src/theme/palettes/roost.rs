//! Roost-owned colour roles that rust-ui has no counterpart for: the terminal
//! surface and ANSI and syntax palettes, and the chat accent. Each appearance
//! has one set; the base and accent palettes never touch them.

/// The dark terminal, syntax and chat roles.
pub const ROOST_DARK: &[(&str, &str)] = &[
    ("--term-bg", "#0a0f11"),
    ("--term-fg", "#e3e3e1"),
    ("--syntax-plain", "#e3e3e1"),
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
    ("--chat-accent", "#ed4abf"),
];

/// The light terminal, syntax and chat roles.
pub const ROOST_LIGHT: &[(&str, &str)] = &[
    ("--term-bg", "#ffffff"),
    ("--term-fg", "#1f1f1f"),
    ("--syntax-plain", "#383a42"),
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
    ("--chat-accent", "#c4268f"),
];

//! Omarchy palettes: one set of raw colours per palette, from which every role
//! the stylesheets read is derived by the rules below. The palette data lives in
//! `palettes/omarchy_a.rs` and `palettes/omarchy_b.rs`. This file owns the rules,
//! so adding a palette is data and never a second copy of the rules.

use super::tokens::ThemeAppearance;

/// One Omarchy palette's raw colours. Fallbacks pi-pocket applies to missing
/// keys are already resolved, so every field is a hex literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OmarchyPalette {
    pub id: &'static str,
    pub label: &'static str,
    /// Omarchy palettes have one appearance, so the mode control does not apply.
    pub appearance: ThemeAppearance,
    pub background: &'static str,
    pub dark_background: &'static str,
    pub darker_background: &'static str,
    pub foreground: &'static str,
    pub dark_foreground: &'static str,
    pub bright_foreground: &'static str,
    pub accent: &'static str,
    pub selection: &'static str,
    pub red: &'static str,
    pub yellow: &'static str,
    pub orange: &'static str,
    pub green: &'static str,
    pub cyan: &'static str,
    pub blue: &'static str,
    pub magenta: &'static str,
    pub bright_yellow: &'static str,
}

/// `percent` of `of` mixed into `into`. Every derived surface is a mix of the
/// foreground into a background, as pi-pocket's style.css builds them.
fn mix(percent: u8, of: &str, into: &str) -> String {
    format!("color-mix(in srgb, {of} {percent}%, {into})")
}

/// The custom properties an Omarchy palette writes: every rust-ui role, the
/// sidebar roles, and Roost's terminal and syntax roles.
pub fn derive_roles(palette: &OmarchyPalette) -> Vec<(&'static str, String)> {
    let p = palette;
    let own = |value: &str| value.to_owned();
    let raised = |percent: u8| mix(percent, p.foreground, p.background);
    let sidenav_raised = |percent: u8| mix(percent, p.foreground, p.dark_background);
    vec![
        ("--background", own(p.background)),
        ("--foreground", own(p.bright_foreground)),
        ("--card", raised(4)),
        ("--card-foreground", own(p.bright_foreground)),
        ("--popover", raised(4)),
        ("--popover-foreground", own(p.bright_foreground)),
        ("--primary", own(p.accent)),
        ("--primary-foreground", own(p.background)),
        ("--primary-light", mix(14, p.accent, p.background)),
        ("--primary-dark", mix(78, p.accent, p.foreground)),
        ("--secondary", raised(8)),
        ("--secondary-foreground", own(p.bright_foreground)),
        ("--muted", raised(6)),
        ("--muted-foreground", own(p.dark_foreground)),
        ("--accent", raised(7)),
        ("--accent-foreground", own(p.bright_foreground)),
        ("--border", raised(13)),
        ("--input", raised(13)),
        ("--ring", own(p.accent)),
        ("--selection", own(p.selection)),
        ("--selection-foreground", own(p.bright_foreground)),
        ("--surface", own(p.darker_background)),
        ("--surface-foreground", own(p.foreground)),
        ("--destructive", own(p.red)),
        ("--destructive-foreground", own(p.background)),
        ("--destructive-light", mix(14, p.red, p.background)),
        ("--destructive-dark", mix(85, p.red, p.foreground)),
        ("--success", own(p.green)),
        ("--success-foreground", own(p.background)),
        ("--success-light", mix(14, p.green, p.background)),
        ("--success-dark", mix(78, p.green, p.foreground)),
        ("--warning", own(p.yellow)),
        ("--warning-foreground", own(p.background)),
        ("--warning-light", mix(14, p.yellow, p.background)),
        ("--warning-dark", mix(62, p.yellow, p.foreground)),
        ("--info", own(p.blue)),
        ("--info-foreground", own(p.background)),
        ("--info-light", mix(14, p.blue, p.background)),
        ("--info-dark", mix(62, p.blue, p.foreground)),
        ("--sidenav", own(p.dark_background)),
        ("--sidenav-foreground", own(p.foreground)),
        ("--sidenav-primary", own(p.accent)),
        ("--sidenav-primary-foreground", own(p.background)),
        ("--sidenav-accent", sidenav_raised(7)),
        ("--sidenav-accent-foreground", own(p.bright_foreground)),
        ("--sidenav-border", sidenav_raised(13)),
        ("--sidenav-ring", own(p.accent)),
        ("--term-bg", own(p.background)),
        ("--term-fg", own(p.foreground)),
        ("--syntax-plain", own(p.foreground)),
        ("--syntax-keyword", mix(50, p.magenta, p.foreground)),
        ("--syntax-string", mix(50, p.green, p.foreground)),
        ("--syntax-number", mix(50, p.orange, p.foreground)),
        ("--syntax-comment", own(p.dark_foreground)),
        ("--ansi-black", own(p.dark_background)),
        ("--ansi-red", own(p.red)),
        ("--ansi-green", own(p.green)),
        ("--ansi-yellow", own(p.yellow)),
        ("--ansi-blue", own(p.blue)),
        ("--ansi-magenta", own(p.magenta)),
        ("--ansi-cyan", own(p.cyan)),
        ("--ansi-white", own(p.foreground)),
        ("--ansi-bright-black", own(p.dark_foreground)),
        ("--ansi-bright-red", own(p.red)),
        ("--ansi-bright-green", own(p.green)),
        ("--ansi-bright-yellow", own(p.bright_yellow)),
        ("--ansi-bright-blue", own(p.blue)),
        ("--ansi-bright-magenta", own(p.magenta)),
        ("--ansi-bright-cyan", own(p.cyan)),
        ("--ansi-bright-white", own(p.bright_foreground)),
    ]
}

//! The inline geometry the workbench shell writes: the grid's height and
//! sidebar-width properties, the editor region's keyboard shift and composer
//! reservation, the `--roost-main-left` offset, and when the compact top bar
//! shows. Ports `shellStyle`, `editorStyle` and the memos of
//! `apps/web/src/components/layout/AppShell.tsx`; read by `AppShell`.
//!
//! A compact terminal route reserves the composer's RESTING row whether or not
//! the composer is mounted, and the soft keyboard only translates the region:
//! a PTY resize makes an inline TUI repaint, and one repainting in place
//! duplicates rows into history. `keyboard_resize` is the explicit opt-in.

/// What the composer is doing, when a composer is mounted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ComposerGeometry {
    /// Whether the composer is open and taking input.
    ///
    /// `Default` is "no composer": the shell that owns the reactive signal is
    /// created before any dock has mounted, and an inactive composer reserving
    /// a measured height would be a reserve for a composer that is not there.
    pub active: bool,
    /// Its measured height in pixels.
    pub height_px: f64,
}

/// The shell's inline style: height, then the three sidebar-width properties.
pub fn shell_style(
    keyboard_resize: bool,
    composer_active: bool,
    sidebar_width_px: u32,
    collapsed: bool,
) -> String {
    let height = if keyboard_resize && !composer_active {
        "calc(100svh - var(--kb-offset))"
    } else {
        "100svh"
    };
    let (width, resizer) = if collapsed {
        ("0px", "0px")
    } else {
        (
            "var(--workbench-sidebar-expanded-width)",
            "var(--workbench-sidebar-resizer-width)",
        )
    };
    format!(
        "height: {height}; --workbench-sidebar-expanded-width: {sidebar_width_px}px; \
         --workbench-sidebar-width: {width}; --workbench-sidebar-resizer-active-width: {resizer};"
    )
}

/// The editor region's inline style.
pub fn editor_style(
    terminal_route: bool,
    compact: bool,
    keyboard_resize: bool,
    composer: ComposerGeometry,
) -> String {
    let base = "--term-chat-growth: 0px;";
    if !terminal_route || keyboard_resize {
        return base.to_owned();
    }
    let shift = "transform: translateY(calc(var(--kb-offset) * -1));";
    if !compact {
        return format!("{base} {shift}");
    }
    let growth = if composer.active {
        format!(
            "max(0px, calc({}px - var(--term-chat-rest-height)))",
            composer.height_px
        )
    } else {
        "0".to_owned()
    };
    format!(
        "{shift} padding-bottom: calc(var(--term-chat-rest-height) + var(--term-chat-dock-rest-offset)); \
         --term-chat-growth: {growth};"
    )
}

/// `data-keyboard-shift`: set on a terminal route unless the reader opted into
/// keyboard resizing.
///
/// The soft keyboard TRANSLATES the editor region and never resizes it, and
/// that has to hold while the composer is active — the active composer is the
/// reason the keyboard is open. Gating this on the composer's state took the
/// translation away the moment the soft keyboard appeared, so the terminal
/// dropped back down under the raised dock while the dock's own offset still
/// counted the inset. The reserve and the shift are ONE decision: both belong
/// to this predicate.
#[must_use]
pub fn keyboard_shift(terminal_route: bool, keyboard_resize: bool) -> bool {
    terminal_route && !keyboard_resize
}

/// `--roost-main-left`: where the editor starts, for fixed overlays that align
/// to it.
pub fn main_left_offset(compact: bool, collapsed: bool, sidebar_width_px: u32) -> String {
    if compact {
        return "0px".to_owned();
    }
    let sidebar = if collapsed {
        "0px".to_owned()
    } else {
        format!("calc({sidebar_width_px}px + var(--workbench-sidebar-resizer-width))")
    };
    format!("calc(var(--workbench-activity-width) + {sidebar})")
}

/// The compact top bar shows off the root, browse, settings and terminal routes
/// (those carry their own chrome).
pub fn shows_mobile_top_bar(compact: bool, pathname: &str, terminal_route: bool) -> bool {
    compact
        && pathname != "/"
        && !pathname.starts_with("/browse")
        && !pathname.starts_with("/settings")
        && !terminal_route
}

/// v2's terminal-route test for the shell: a prefix, so a terminal route that
/// has not resolved yet still shifts for the keyboard.
pub fn is_terminal_path(pathname: &str) -> bool {
    pathname.starts_with("/s/") || pathname.starts_with("/t/") || pathname.starts_with("/w/")
}

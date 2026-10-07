//! Which body-floating surfaces one pane mounts: the compact composer dock and
//! the touch/controller terminal keys — as the floating sheet, or on a
//! television as the input tray in the pane composer's bar — plus the display
//! lift that keeps both clear of the terminal's bottom rows.
//!
//! They live here rather than in `cell_terminal` because they are the same
//! decision asked twice with different shells, and the pane's own file has no
//! room left for the reasoning. The composer follows the compact layout alone;
//! the keys also answer to a directional modality, because a remote or a pad
//! has no other way to send Esc, Tab, Ctrl-<key> or PageUp to the PTY.
//!
//! Both keep the drawer's and an overlay route's exclusions: a fixed surface
//! over a hidden terminal is a control the reader cannot act on.

/// Whether the compact shell should mount the portaled composer dock for this
/// pane.
///
/// The same five conditions v2 gates on: the pane is in the layout, it is the
/// focused one, the shell is compact, the drawer is not over it, and its
/// surface is actually visible. A composer portaled over a hidden surface is a
/// composer the user cannot see and cannot dismiss.
pub fn mounts_viewport_composer(
    in_layout: bool,
    focused: bool,
    compact: bool,
    drawer_open: bool,
    surface_visible: bool,
) -> bool {
    in_layout && focused && compact && !drawer_open && surface_visible
}

/// Whether the key sheet and its toggle mount for this pane.
///
/// The composer's conditions with the compact shell widened to any shell a TV
/// remote or controller drives.
pub fn mounts_nav_pad(
    in_layout: bool,
    focused: bool,
    compact: bool,
    directional_input_active: bool,
    drawer_open: bool,
    surface_visible: bool,
) -> bool {
    in_layout && focused && (compact || directional_input_active) && !drawer_open && surface_visible
}

/// Where a pane's terminal keys live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySurface {
    /// This pane shows no keys.
    Hidden,
    /// The floating sheet and its corner toggle.
    Sheet,
    /// The TV input tray inside the pane composer's bar: on a television the
    /// keys, the text field, the mic and Send are one surface at the bottom of
    /// the terminal rather than a sheet floating in a corner.
    Tray,
}

/// Where the keys go, given `mounts_nav_pad`'s answer and whether the
/// ten-foot layout is on (which always has the pane composer to host a tray).
pub fn key_surface(mounts_keys: bool, tv_layout: bool) -> KeySurface {
    match (mounts_keys, tv_layout) {
        (false, _) => KeySurface::Hidden,
        (true, false) => KeySurface::Sheet,
        (true, true) => KeySurface::Tray,
    }
}

/// The display's `transform`. A grown composer and an open tray both cover the
/// bottom of the display, so it is lifted by both rather than shrunk: a PTY
/// height change makes an inline TUI repaint. Never empty — Dioxus keeps an
/// inline property the new style string omits, so a shrink would leave the
/// display lifted.
pub fn display_lift(composer_growth_px: u32, tray_open: bool) -> String {
    match (composer_growth_px, tray_open) {
        (0, false) => "none".to_owned(),
        (growth, false) => format!("translateY(-{growth}px)"),
        (growth, true) => format!("translateY(calc(-{growth}px - var(--term-tray-height)))"),
    }
}

/// Whether arming the on-screen Ctrl latch must first take the terminal's
/// focus.
///
/// A device with a pointer already holds the pane's focus, and a directional
/// modality is driving the terminal from its own remote or pad, so neither
/// needs a focus move — and a focus move is exactly what the sheet's
/// mouse-down `preventDefault` exists to avoid.
pub fn ctrl_arm_takes_focus(
    armed: bool,
    touch_device: bool,
    directional_input_active: bool,
) -> bool {
    armed && !touch_device && !directional_input_active
}

#[cfg(test)]
mod tests {
    use super::{
        KeySurface, ctrl_arm_takes_focus, display_lift, key_surface, mounts_nav_pad,
        mounts_viewport_composer,
    };

    #[test]
    fn the_portaled_dock_appears_only_for_the_focused_visible_pane_on_a_compact_shell() {
        assert!(mounts_viewport_composer(true, true, true, false, true));
    }

    #[test]
    fn a_pane_outside_the_layout_never_gets_the_portaled_dock() {
        // Parked panes stay mounted on desktop; a second portaled dock would
        // leave two composers fighting over one viewport row.
        assert!(!mounts_viewport_composer(false, true, true, false, true));
    }

    #[test]
    fn a_pane_that_is_not_focused_never_gets_the_portaled_dock() {
        assert!(!mounts_viewport_composer(true, false, true, false, true));
    }

    #[test]
    fn a_desktop_shell_mounts_the_pane_placement_instead() {
        assert!(!mounts_viewport_composer(true, true, false, false, true));
    }

    #[test]
    fn the_drawer_unmounts_the_composer_while_it_is_open() {
        assert!(!mounts_viewport_composer(true, true, true, true, true));
    }

    #[test]
    fn a_hidden_surface_never_gets_the_portaled_dock() {
        assert!(!mounts_viewport_composer(true, true, true, false, false));
    }

    #[test]
    fn a_compact_focused_visible_pane_gets_the_key_sheet() {
        assert!(mounts_nav_pad(true, true, true, false, false, true));
    }

    #[test]
    fn a_directional_modality_gets_keys_without_a_compact_layout() {
        // A remote or a pad has no other way to send Esc, Tab or PageUp.
        assert!(mounts_nav_pad(true, true, false, true, false, true));
    }

    #[test]
    fn a_television_gets_the_tray_and_every_other_shell_the_sheet() {
        assert_eq!(key_surface(true, true), KeySurface::Tray);
        assert_eq!(key_surface(true, false), KeySurface::Sheet);
        assert_eq!(key_surface(false, true), KeySurface::Hidden);
    }

    #[test]
    fn an_open_tray_lifts_the_display_on_top_of_the_composer_growth() {
        assert_eq!(display_lift(0, false), "none");
        assert_eq!(display_lift(12, false), "translateY(-12px)");
        assert_eq!(
            display_lift(0, true),
            "translateY(calc(-0px - var(--term-tray-height)))"
        );
    }

    #[test]
    fn a_plain_desktop_shell_never_gets_the_sheet() {
        assert!(!mounts_nav_pad(true, true, false, false, false, true));
    }

    #[test]
    fn the_drawer_unmounts_the_sheet_while_it_is_open() {
        assert!(!mounts_nav_pad(true, true, true, true, true, true));
    }

    #[test]
    fn a_hidden_surface_never_gets_the_sheet() {
        assert!(!mounts_nav_pad(true, true, true, true, false, false));
    }

    #[test]
    fn a_parked_or_unfocused_pane_never_gets_the_sheet() {
        assert!(!mounts_nav_pad(false, true, true, true, false, true));
        assert!(!mounts_nav_pad(true, false, true, true, false, true));
    }

    #[test]
    fn arming_ctrl_takes_the_focus_only_where_no_pointer_already_owns_it() {
        assert!(ctrl_arm_takes_focus(true, false, false));
    }

    #[test]
    fn a_touch_device_or_a_directional_modality_already_owns_the_focus() {
        assert!(!ctrl_arm_takes_focus(true, true, false));
        assert!(!ctrl_arm_takes_focus(true, false, true));
    }

    #[test]
    fn disarming_never_takes_the_focus() {
        assert!(!ctrl_arm_takes_focus(false, false, false));
        assert!(!ctrl_arm_takes_focus(false, true, true));
    }
}

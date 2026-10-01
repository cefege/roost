//! Which body-floating surfaces one pane mounts: the compact composer dock and
//! the touch/controller terminal key sheet.
//!
//! Both live here rather than in `cell_terminal` because they are the same
//! decision asked twice with different shells, and the pane's own file has no
//! room left for the reasoning. The composer follows the compact layout alone;
//! the key sheet also answers to a directional modality, because a television
//! has no compact layout and no soft keyboard and the sheet is the only way its
//! D-pad can send Esc, Tab, Ctrl-<key> or PageUp to the PTY.
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
    use super::{ctrl_arm_takes_focus, mounts_nav_pad, mounts_viewport_composer};

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
    fn a_television_gets_the_sheet_without_a_compact_layout() {
        // The TV's only raw-key surface; the pane composer does not mount there.
        assert!(mounts_nav_pad(true, true, false, true, false, true));
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

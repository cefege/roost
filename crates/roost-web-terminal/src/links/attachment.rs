//! The terminal link attachment: physical-modifier hover, the armed paint hold,
//! the floating hint, and physical or compact-sheet activation of painted and
//! inferred anchors, composed with the mutation scanner. It runs over
//! `LinkHost`; `links::dom` wires the browser events of `LinkListener::ALL`
//! into `dispatch`. The pane component attaches one per terminal.
//! Ports `attachTerminalLinks` in `apps/web/src/renderer/terminal-links.ts`.

mod listener;

pub use listener::{LinkEvent, LinkHost, LinkListener};

use super::TerminalLinkOptions;
use super::activation::{is_link_activation_gesture, is_link_modifier_held, link_hint_text};
use super::anchor::{apply_terminal_anchor_target, resolve_terminal_anchor_target};
use super::scan::{FrameCallback, LinkScanner};
use crate::cell_row::TERMINAL_LINK_TARGET_ATTR;
use crate::link_target::TerminalLinkTarget;

/// The container attribute the link stylesheet keys its underline on.
const LINK_ARMED_ATTR: &str = "data-link-armed";

/// One pane's link attachment.
pub struct TerminalLinks<H: LinkHost> {
    host: H,
    options: TerminalLinkOptions,
    scanner: LinkScanner<H>,
    active: bool,
    disposed: bool,
    armed: bool,
    pointer_inside: bool,
    holding: bool,
    interaction_listeners_attached: bool,
}

impl<H: LinkHost> TerminalLinks<H> {
    /// Attach; a pane that starts hidden installs no link work.
    pub fn attach(host: H, options: TerminalLinkOptions) -> Self {
        let initial_active = options.initial_active;
        let scanner = LinkScanner::attach(&host, initial_active);
        let mut links = Self {
            host,
            options,
            scanner,
            active: initial_active,
            disposed: false,
            armed: false,
            pointer_inside: false,
            holding: false,
            interaction_listeners_attached: false,
        };
        if initial_active {
            links.attach_interaction_listeners();
        }
        links
    }

    /// The host, for the adapter's frame and observer wiring.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Route one listener's event. Returns whether to `preventDefault`.
    pub fn dispatch(&mut self, listener: LinkListener, event: LinkEvent<H::Element>) -> bool {
        match listener {
            LinkListener::KeyDown | LinkListener::KeyUp => {
                if event.key.as_deref() == Some(self.options.modifier_key.event_key()) {
                    self.set_armed(listener == LinkListener::KeyDown);
                }
            }
            LinkListener::Blur => self.release_interaction(),
            LinkListener::MouseOver => {
                self.set_armed(is_link_modifier_held(
                    &event.gesture,
                    self.options.modifier_key,
                ));
                if self.armed {
                    match &event.anchor {
                        Some(anchor) => self.show_hint(anchor),
                        None => self.host.hide_hint(),
                    }
                }
            }
            LinkListener::MouseOut => {
                if event.anchor.is_some() {
                    self.host.hide_hint();
                }
            }
            LinkListener::MouseEnter => {
                self.pointer_inside = true;
                self.set_armed(is_link_modifier_held(
                    &event.gesture,
                    self.options.modifier_key,
                ));
                self.recompute_hold();
            }
            LinkListener::MouseLeave => {
                self.pointer_inside = false;
                self.recompute_hold();
            }
            // Every pointer event carries the LIVE modifier level, so it
            // re-derives `armed` in both directions: a keyup delivered elsewhere
            // must never strand the hold.
            LinkListener::MouseMove | LinkListener::MouseDown => {
                self.set_armed(is_link_modifier_held(
                    &event.gesture,
                    self.options.modifier_key,
                ));
            }
            LinkListener::Click => return self.click(&event),
        }
        false
    }

    /// An animation frame the scanner requested fired.
    pub fn frame_fired(&mut self, callback: FrameCallback) {
        match callback {
            FrameCallback::Scan => self.scanner.scan(&self.host, &self.options),
            FrameCallback::ActivationScan => self.scanner.activation_frame_fired(&self.host),
        }
    }

    /// The idle callback the scanner requested fired.
    pub fn idle_scan_fired(&mut self) {
        self.scanner.scan(&self.host, &self.options);
    }

    /// A mutation batch arrived.
    pub fn mutations_observed(&mut self, touched_rows: impl FnOnce() -> Vec<H::Element>) {
        self.scanner.observe_mutations(&self.host, touched_rows);
    }

    /// The document's visibility changed.
    pub fn visibility_changed(&mut self) {
        self.scanner.visibility_changed(&self.host);
    }

    /// Clear the modifier and pointer levels, releasing any hold.
    pub fn release_interaction(&mut self) {
        self.host
            .remove_attribute(&self.host.container(), LINK_ARMED_ATTR);
        self.armed = false;
        self.pointer_inside = false;
        self.recompute_hold();
        self.host.hide_hint();
    }

    /// The display text of the target `anchor` opens, or `None`.
    pub fn describe_link(&self, anchor: &H::Element) -> Option<String> {
        resolve_terminal_anchor_target(&self.host, anchor, self.options.file_resolver())
            .map(|target| target.display().to_string())
    }

    /// Open `anchor` as the context menu does. Returns whether it opened.
    pub fn open_link(&mut self, anchor: &H::Element) -> bool {
        let Some(target) =
            resolve_terminal_anchor_target(&self.host, anchor, self.options.file_resolver())
        else {
            return false;
        };
        let raw_target = self.author_anchor(anchor, &target);
        match &target {
            TerminalLinkTarget::WorkerFile { href, .. } => {
                let (Some(href), Some(open_file)) = (href, &self.options.on_open_file) else {
                    return false;
                };
                tracing::info!(target: "terminal_links", "opening a terminal file link");
                open_file(href);
            }
            TerminalLinkTarget::External { .. } => {
                let Some(native) = self.host.create_anchor() else {
                    return false;
                };
                apply_terminal_anchor_target(
                    &self.host,
                    &native,
                    &raw_target,
                    &target,
                    None,
                    self.options.modifier_key,
                );
                self.host.set_attribute(&native, "style", "display:none");
                tracing::info!(target: "terminal_links", "opening an external terminal link");
                self.host.click_detached_anchor(&native);
            }
        }
        self.host.hide_hint();
        true
    }

    /// Foreground (`true`) or withdraw (`false`) the pane's link work.
    pub fn set_active(&mut self, next_active: bool) {
        if self.disposed || next_active == self.active {
            return;
        }
        self.active = next_active;
        tracing::debug!(target: "terminal_links", active = next_active, "link attachment activity");
        if !next_active {
            self.detach_interaction_listeners();
            self.release_interaction();
            self.scanner.set_active(&self.host, false);
            return;
        }
        self.scanner.set_active(&self.host, true);
        self.attach_interaction_listeners();
    }

    /// Tear down for good.
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.set_active(false);
        self.disposed = true;
        self.scanner.dispose(&self.host);
        self.host.remove_hint();
    }

    fn click(&mut self, event: &LinkEvent<H::Element>) -> bool {
        let Some(anchor) = &event.anchor else {
            return false;
        };
        let armed = self
            .options
            .link_activation_armed
            .as_ref()
            .is_some_and(|armed| armed());
        let target =
            resolve_terminal_anchor_target(&self.host, anchor, self.options.file_resolver());
        let Some(target) = target.filter(|_| {
            is_link_activation_gesture(&event.gesture, armed, self.options.modifier_key)
        }) else {
            return true;
        };
        self.author_anchor(anchor, &target);
        let mut prevent_default = false;
        if let TerminalLinkTarget::WorkerFile { href, .. } = &target {
            prevent_default = true;
            if let (Some(href), Some(open_file)) = (href, &self.options.on_open_file) {
                tracing::info!(target: "terminal_links", "opening a terminal file link");
                open_file(href);
            }
        }
        self.host.hide_hint();
        prevent_default
    }

    /// Re-author `anchor` for its freshly resolved target; returns the raw target.
    fn author_anchor(&self, anchor: &H::Element, target: &TerminalLinkTarget) -> String {
        let raw_target = self
            .host
            .attribute(anchor, TERMINAL_LINK_TARGET_ATTR)
            .unwrap_or_else(|| target.display().to_string());
        apply_terminal_anchor_target(
            &self.host,
            anchor,
            &raw_target,
            target,
            None,
            self.options.modifier_key,
        );
        raw_target
    }

    fn show_hint(&self, anchor: &H::Element) {
        let Some(hint) = self
            .host
            .attribute(anchor, "data-hint")
            .filter(|hint| !hint.is_empty())
        else {
            return;
        };
        self.host
            .show_hint(anchor, &link_hint_text(self.options.modifier_key, &hint));
    }

    fn set_armed(&mut self, next: bool) {
        if next == self.armed {
            return;
        }
        self.armed = next;
        let container = self.host.container();
        if next {
            self.host.set_attribute(&container, LINK_ARMED_ATTR, "1");
        } else {
            self.host.remove_attribute(&container, LINK_ARMED_ATTR);
        }
        self.recompute_hold();
    }

    fn recompute_hold(&mut self) {
        let next = self.armed && self.pointer_inside;
        if next == self.holding {
            return;
        }
        self.holding = next;
        tracing::debug!(target: "terminal_links", holding = next, "link armed hold");
        if let Some(changed) = &self.options.on_armed_hover_change {
            changed(next);
        }
        // Repaint can have replaced inferred anchors in the current tail.
        if self.active && self.armed {
            self.scanner.request_current_scan(&self.host);
        }
    }

    fn attach_interaction_listeners(&mut self) {
        if self.interaction_listeners_attached {
            return;
        }
        self.interaction_listeners_attached = true;
        LinkListener::ALL
            .into_iter()
            .for_each(|listener| self.host.add_listener(listener));
    }

    fn detach_interaction_listeners(&mut self) {
        if !self.interaction_listeners_attached {
            return;
        }
        self.interaction_listeners_attached = false;
        LinkListener::ALL
            .into_iter()
            .for_each(|listener| self.host.remove_listener(listener));
    }
}

/// JS `Math.round`: halves round toward positive infinity.
pub fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

impl<H: LinkHost> std::fmt::Debug for TerminalLinks<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalLinks")
            .field("scanner", &self.scanner)
            .field("active", &self.active)
            .field("armed", &self.armed)
            .field("pointer_inside", &self.pointer_inside)
            .field("holding", &self.holding)
            .finish()
    }
}

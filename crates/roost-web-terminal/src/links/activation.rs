//! Turning a painted terminal link into the action a click performs, and the
//! gesture rule that decides which click counts as one.
//!
//! The target string is never re-parsed here: `classify_terminal_link_target`
//! is the ONE authority on what a terminal-authored target is, and this module
//! takes its answer verbatim. That is deliberate. v2 delegated its URL parsing
//! to `new URL()`, and a hand-rolled authority parser is what panicked on a
//! bracketed IPv6 literal like `http://[::1]:4103/x` — one byte past the
//! authority — during this port. An activation path that re-derived the host,
//! the port or the path from the raw string would reintroduce it.
//!
//! The one gate this module owns is `is_worker_file_href`: a route Roost itself
//! minted. That is a check on OUR output, not a second classifier of terminal
//! input. Ported from v2's `terminal-links.ts` and `terminal-links.target.ts`.

use roost_protocol::cell::link_uri_within_cap;

use super::PaintedLinkAttributes;
use crate::link_target::{TerminalLinkTarget, classify_terminal_link_target};
use crate::reader_intent::HoldChange;

/// The physical modifier key a platform opens a terminal link with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkModifierKey {
    /// Everywhere but macOS: Ctrl-click.
    Control,
    /// macOS: Command-click.
    Meta,
}

impl LinkModifierKey {
    /// The key for a platform, which is the one thing that decides it.
    pub const fn for_platform(is_macos: bool) -> Self {
        if is_macos { Self::Meta } else { Self::Control }
    }

    /// The key as the hover hint names it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Control => "Ctrl",
            Self::Meta => "⌘",
        }
    }
}

/// A click as the activation predicate sees it. Physical modifiers come from
/// the event; compact arming is pane-local state and is a parameter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkActivationGesture {
    /// DOM `MouseEvent.button`; only 0 activates.
    pub button: i16,
    /// Ctrl held.
    pub ctrl: bool,
    /// Command held.
    pub meta: bool,
    /// Shift, which selects text instead of opening anything.
    pub shift: bool,
    /// Alt, which selects text instead of opening anything.
    pub alt: bool,
}

/// Whether a click activates a terminal link rather than selecting terminal text.
///
/// Compact arming wins outright: a phone has no physical modifier to press, so
/// the pane's own armed state is the gesture. With a physical key the platform
/// decides which one it is, and holding BOTH is not the gesture — that is how a
/// two-finger shortcut stays available.
pub fn is_link_activation_gesture(
    gesture: &LinkActivationGesture,
    activation_armed: bool,
    modifier_key: LinkModifierKey,
) -> bool {
    if gesture.button != 0 || gesture.shift || gesture.alt {
        return false;
    }
    if activation_armed {
        return true;
    }
    match modifier_key {
        LinkModifierKey::Meta => gesture.meta && !gesture.ctrl,
        LinkModifierKey::Control => gesture.ctrl && !gesture.meta,
    }
}

/// The hint text a hovered link shows, naming the key that opens it.
///
/// The `Open ` prefix the DOM applier adds to a file target is stripped here so
/// the hint does not read "to open · Open src/main.rs".
pub fn link_hint(modifier_key: LinkModifierKey, display: &str) -> String {
    let display = display.strip_prefix("Open ").unwrap_or(display);
    format!("{}-click to open · {display}", modifier_key.label())
}

/// What activating a painted terminal link does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkActivation {
    /// Open this absolute HTTP(S) URL. The href is the terminal-authored target
    /// VERBATIM: a re-serialized URL would retarget the click.
    OpenExternal {
        /// The href to hand the browser.
        href: String,
        /// The hover text, which is the same target.
        display: String,
    },
    /// Route to this path in the file viewer.
    OpenWorkerFile {
        /// The authenticated in-app route, `/file/<workerFp>/…`.
        href: String,
        /// The terminal-authored target, which is the hover text.
        display: String,
    },
}

impl LinkActivation {
    /// The terminal-authored target, which is also the hover text.
    pub fn display(&self) -> &str {
        match self {
            Self::OpenExternal { display, .. } | Self::OpenWorkerFile { display, .. } => {
                display
            }
        }
    }
}

/// The title a hovered anchor carries: the key, spelled out, and the target.
///
/// Spelled out rather than glyphed because it is the attribute a screen reader
/// and a status-bar reader both fall back to, where "⌘" is a riddle.
pub fn link_title(modifier_key: LinkModifierKey, display: &str) -> String {
    let key = match modifier_key {
        LinkModifierKey::Control => "Control",
        LinkModifierKey::Meta => "Command",
    };
    format!("{key}-click to open {display}")
}

/// The action a click on this painted link performs, or `None` when it must not
/// open at all.
///
/// `resolve_file` is the pane's worker-aware file resolver, and it is what turns
/// a printed path into a route. A file target with NO resolver is refused here
/// rather than handed on with a null href: the browser has no route for a path,
/// and the app has no worker to ask.
pub fn activate_link<F>(
    attributes: &PaintedLinkAttributes,
    resolve_file: Option<F>,
) -> Option<LinkActivation>
where
    F: Fn(&str, Option<u32>, Option<&str>) -> Option<String>,
{
    if !attributes.is_terminal_link {
        return None;
    }
    // A link that already carries a resolved route still names its own target
    // first, so re-resolution always sees the terminal-authored string.
    let raw_target = attributes.target.as_deref()?;
    let target = classify_terminal_link_target(raw_target)?;
    match target {
        TerminalLinkTarget::External { href, display } => Some(LinkActivation::OpenExternal {
            href,
            display,
        }),
        TerminalLinkTarget::WorkerFile {
            raw_path,
            line,
            file_authority,
            display,
        } => {
            let resolve = resolve_file?;
            let href = resolve(&raw_path, line, file_authority.as_deref())?;
            if !is_worker_file_href(&href) {
                return None;
            }
            Some(LinkActivation::OpenWorkerFile { href, display })
        }
    }
}

/// Only a route this build minted may be installed on an internal terminal
/// anchor: `/file/<workerFp>/<path>`, optionally with an `#L<line>` fragment. A
/// query string or any other fragment is not one, and neither is a route over
/// the wire cap — a click must not be retargetable by a 2 KB string.
pub fn is_worker_file_href(href: &str) -> bool {
    if !link_uri_within_cap(href) {
        return false;
    }
    let Some(rest) = href.strip_prefix("/file/") else {
        return false;
    };
    let Some((worker_fp, tail)) = rest.split_once('/') else {
        return false;
    };
    if worker_fp.is_empty() || worker_fp.contains(['?', '#']) {
        return false;
    }
    let Some((path, fragment)) = tail.split_once('#') else {
        return !tail.is_empty() && !tail.contains('?');
    };
    !path.is_empty()
        && !path.contains('?')
        && fragment.strip_prefix('L').is_some_and(is_line_fragment)
}

/// The 1-based line, with no leading zero: the viewer's only fragment contract.
fn is_line_fragment(digits: &str) -> bool {
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && !digits.starts_with('0')
}

/// Why a press was not forwarded to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressWithheld {
    /// It is the exact local link gesture, which the terminal itself owns.
    LinkActivation,
    /// The middle button, reserved for the deck's bring-to-front toggle.
    DeckMiddleButton,
}

/// Whether a press is withheld from the application, and why.
///
/// The exact local link gesture wins over DECSET mouse reporting. A bare anchor
/// click falls through, so a mouse-aware TUI keeps its press inside a link.
pub fn withhold_press(
    over_terminal_link: bool,
    gesture: &LinkActivationGesture,
    activation_armed: bool,
    modifier_key: LinkModifierKey,
    button_is_middle: bool,
) -> Option<PressWithheld> {
    if over_terminal_link
        && is_link_activation_gesture(gesture, activation_armed, modifier_key)
    {
        return Some(PressWithheld::LinkActivation);
    }
    if button_is_middle {
        return Some(PressWithheld::DeckMiddleButton);
    }
    None
}

/// The level a link modifier hold is derived from, and the hold it drives.
///
/// Both inputs are LEVELS, not edges, so the hold can never outlive the state
/// that justifies it. Raising on one pointer event and lowering only on the
/// keyup edge strands the hold whenever that keyup is delivered somewhere else —
/// an OS app switch, a swallowed key — and every pointer event re-derives it in
/// BOTH directions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkArmedHold {
    /// The modifier (or the compact arming) is down.
    armed: bool,
    /// The pointer is inside the pane.
    pointer_inside: bool,
    /// The last level handed to the renderer.
    holding: bool,
}

impl LinkArmedHold {
    /// The armed level.
    pub fn set_armed(&mut self, armed: bool) -> HoldChange {
        self.armed = armed;
        self.recompute()
    }

    /// The pointer level.
    pub fn set_pointer_inside(&mut self, inside: bool) -> HoldChange {
        self.pointer_inside = inside;
        self.recompute()
    }

    /// Drop both levels — a window blur, or a pane going inactive.
    pub fn release(&mut self) -> HoldChange {
        self.armed = false;
        self.pointer_inside = false;
        self.recompute()
    }

    /// Whether the renderer is held right now.
    pub const fn holding(&self) -> bool {
        self.holding
    }

    fn recompute(&mut self) -> HoldChange {
        let next = self.armed && self.pointer_inside;
        if next == self.holding {
            return HoldChange::Unchanged;
        }
        self.holding = next;
        if next {
            HoldChange::Armed
        } else {
            HoldChange::Released
        }
    }
}

//! What the shell needs to know about the portaled composer: whether one is
//! mounted, and how tall it actually is.
//!
//! The height is MEASURED, not declared. A composer whose reserved row was a
//! constant would be wrong the moment the dock grew — and the dock grows for
//! ordinary reasons, a status line or a two-line draft. The shell reserves the
//! resting row plus the measured growth, and the measurement is the only honest
//! source for the second term.
//!
//! REACTIVITY. A `thread_local` is invisible to the virtual DOM, so the slot
//! alone would leave a shell reading a height the dock measured one render
//! late. The app root installs a `Signal` for the same value and this module
//! publishes to both, which is what makes a shell reading `published_geometry`
//! during its own render SUBSCRIBED to the dock. A component mounted outside
//! the root — a native test — finds no signal installed and reads the slot,
//! which is the same value.
//!
//! WHY THIS IS ONE OWNED SLOT AND NOT PER-INSTANCE STATE. The compact shell
//! mounts the portaled dock, and the drawer covers that dock and uncovers it
//! without unmounting it, so two instances can briefly coexist across a
//! responsive swap. The publisher therefore carries a token and only the token
//! that currently owns the slot may publish — the same instance-token rule v2
//! keeps in `TerminalComposeButton.tsx`'s `activeViewportToken`. Without it a
//! disposing dock's zero height clears the replacement's measurement, and the
//! shell's reserve goes stuck for as long as the page lives.
//!
//! A CLAIM IS A REFERENCE, NOT A VALUE. `use_hook` hands every render its own
//! copy of the hook it stores, and that copy dies with the render, so a release
//! hung on the value would hand the slot back while the dock that took it is
//! still on screen — which the shell cannot tell from "no composer is
//! mounted". It hangs on an `Rc` instead, and the release runs when the last
//! handle goes.
//! Ports `composerActive` / `composerHeightPx` and `activeViewportToken` from
//! `apps/web/src/components/terminal/TerminalComposeButton.tsx`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

// `Signal::set` lives on the writable half of the signal API, and this
// module names its signal type in full inside a `thread_local`, so the trait
// has to be brought in by name rather than picked up from a glob.
use dioxus::prelude::{Signal, WritableExt as _};

use crate::components::layout::shell_style::ComposerGeometry;

/// One dock's hold on the slot, and the element it measures. Split out because
/// the two have different lifetimes: the claim ends with the dock's
/// VISIBILITY, the element with the dock's own node.
#[path = "composer_slot.rs"]
mod composer_slot;

pub use composer_slot::ComposerSlot;

thread_local! {
    /// The token of the dock that currently owns the slot. `0` is nobody, so a
    /// real owner token — a monotonically increasing counter — is never
    /// confused with the empty slot.
    static OWNER: Cell<u64> = const { Cell::new(0) };
    /// Every claim that is still mounted, oldest first, so releasing the owner
    /// can HAND THE SLOT BACK rather than clear it. See [`ComposerClaim::claim`].
    static HELD: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// The next token to hand out. Starts at 1 so `0` stays reserved.
    static NEXT_TOKEN: Cell<u64> = const { Cell::new(1) };
    /// The measured dock height, and whether a dock is mounted at all. A
    /// `RefCell` rather than a `Cell` because the geometry is not `Eq`, which
    /// `Cell::const_new` requires and a `const` block cannot spell around.
    static PUBLISHED: RefCell<ComposerGeometry> = const {
        RefCell::new(ComposerGeometry {
            active: false,
            height_px: 0.0,
        })
    };
    /// The root's signal for the same value, installed once by `app`.
    ///
    /// THE SLOT ALONE IS NOT REACTIVE, and that is the one thing this module
    /// cannot fix by itself: a `thread_local` is invisible to the virtual DOM,
    /// so a shell reading only the slot re-renders on the next unrelated
    /// change and the compact reserve lags a frame behind the dock that
    /// measured it. `app.rs` owns the root, so it owns installing this.
    ///
    /// `Option` because a component that mounts without the root — a native
    /// test, a component rendered outside `app` — must still be able to ask and
    /// get the slot's value rather than a panic.
    static SIGNAL: RefCell<Option<Signal<ComposerGeometry>>> = const { RefCell::new(None) };
}

/// Install the root's signal, so a shell reading the geometry during render is
/// SUBSCRIBED to it. Called once, by the app root, before the first dock
/// publishes.
pub fn install_signal(signal: Signal<ComposerGeometry>) {
    SIGNAL.with(|installed| *installed.borrow_mut() = Some(signal));
}

/// A dock's claim on the shell's composer slot.
///
/// Released when the dock goes away. A token rule alone is not enough: two
/// docks can coexist across a responsive swap, and the one that claimed LAST
/// is the one that unmounts FIRST on the way back. Clearing the slot then would
/// leave a visibly mounted composer with the compact INACTIVE lift reserved
/// above it, which is the state the shell cannot tell from "no composer is
/// mounted". So every live claim is REGISTERED, and releasing the owner hands
/// the slot to the newest survivor rather than to nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerClaim {
    token: u64,
    release: Rc<ClaimRelease>,
}

/// The one handle that releases a claim, and the only thing that does.
///
/// The release hangs off a REFERENCE, not off the value: a dock holds its
/// claim by value in `use_hook`, which hands every render its own copy, and
/// that copy dies at the end of the render. Releasing on the copy would hand
/// the slot back while the dock that took it is still on screen — which is the
/// one state the shell reads as "no composer is mounted", so the compact shell
/// reserves only its resting row and the notification dock drops onto the
/// composer instead of above it.
#[derive(Debug, PartialEq, Eq)]
struct ClaimRelease {
    token: u64,
}

impl Drop for ClaimRelease {
    fn drop(&mut self) {
        release_slot(self.token);
    }
}

/// Whether `token` is still the dock the slot answers to.
fn owns_slot(token: u64) -> bool {
    OWNER.with(|owner| owner.get() == token)
}

/// Hand the slot back, to the newest survivor when there is one and to nobody
/// when there is not.
fn release_slot(token: u64) {
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        held.retain(|held| *held != token);
    });
    if !owns_slot(token) {
        return;
    }
    // The newest claim that is STILL MOUNTED takes the slot. Its first
    // measurement is the height the released owner was holding — the two
    // are the same dock across a responsive swap — and its own observer
    // corrects it from there.
    let successor = HELD.with(|held| held.borrow().last().copied()).unwrap_or(0);
    OWNER.with(|owner| owner.set(successor));
    let geometry = if successor == 0 {
        ComposerGeometry {
            active: false,
            height_px: 0.0,
        }
    } else {
        ComposerGeometry {
            active: true,
            height_px: PUBLISHED.with(|published| published.borrow().height_px),
        }
    };
    publish_to_all(geometry);
    tracing::debug!(target: "composer", token, successor, "viewport composer released the shell slot");
}

impl ComposerClaim {
    /// Claim the slot, publishing "a composer is mounted" immediately.
    ///
    /// Taken at MOUNT rather than after the first measurement, because the
    /// shell's `data-keyboard-shift` and its height must both move with the
    /// dock rather than a tick after it.
    pub fn claim() -> Self {
        let token = NEXT_TOKEN.with(|next| {
            let token = next.get();
            next.set(token + 1);
            token
        });
        OWNER.with(|owner| owner.set(token));
        HELD.with(|held| held.borrow_mut().push(token));
        let carried = PUBLISHED.with(|published| published.borrow().height_px);
        publish_to_all(ComposerGeometry {
            active: true,
            height_px: carried,
        });
        tracing::debug!(target: "composer", token, "viewport composer claimed the shell slot");
        Self {
            token,
            release: Rc::new(ClaimRelease { token }),
        }
    }

    /// Publish this dock's measured height, if this dock still owns the slot.
    pub fn publish(&self, height_px: f64) {
        publish_measured(self.token, height_px);
    }

    /// The dock's identity, for a holder that must outlive the claim's own
    /// reference — the resize observer below.
    #[must_use]
    pub const fn token(&self) -> u64 {
        self.token
    }
}

/// Publish a dock's measured height under its token, if that dock still owns
/// the slot.
fn publish_measured(token: u64, height_px: f64) {
    if token == 0 || !owns_slot(token) {
        return;
    }
    publish_to_all(ComposerGeometry {
        active: true,
        height_px,
    });
}

/// What the shell should reserve and shift by right now.
#[must_use]
pub fn published_geometry() -> ComposerGeometry {
    // Reading the SIGNAL here, rather than only the slot, is what subscribes
    // the calling component: a `thread_local` read cannot re-render anything, so
    // a shell reading only the slot would see the new height on whatever change
    // happened to come next. When no signal is installed — a native test, a
    // component outside the root — the slot is the answer.
    if let Some(signal) = SIGNAL.with(|installed| *installed.borrow()) {
        return signal();
    }
    PUBLISHED.with(|published| *published.borrow())
}

/// Publish to both the slot and the signal, so the shell's next read is a
/// reactive one and the value it sees is the value the dock measured.
fn publish_to_all(geometry: ComposerGeometry) {
    PUBLISHED.with(|published| published.replace(geometry));
    // `set` takes `&mut self`, so the copied signal needs its own binding.
    if let Some(mut signal) = SIGNAL.with(|installed| *installed.borrow()) {
        signal.set(geometry);
    }
}

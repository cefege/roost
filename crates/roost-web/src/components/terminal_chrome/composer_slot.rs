//! One dock's hold on the shell's composer slot, and the element it measures,
//! across every time the dock is and is not on screen.
//!
//! The hold cannot be tied to the component's lifetime — the drawer covers the
//! dock and uncovers it without unmounting it — so it is tied to VISIBILITY,
//! which is what the shell's reserve and the notification dock's lift actually
//! describe. The element lives here rather than being passed around because a
//! dock that comes back on screen has to measure itself again, and because the
//! observer reports under the dock's LIVE identity: it outlives every claim the
//! dock makes.
//!
//! The immediate measurement is v2's `publishDockHeight(el)` inside
//! `mountDock` (`apps/web/src/components/terminal/TerminalComposeButton.tsx`):
//! the dock's own ref measures the node it was just handed, because a ref can
//! run before the element is laid out and a later observer delivery is a frame
//! the shell has already reserved from.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::MountedData;
#[cfg(target_arch = "wasm32")]
use dioxus::web::WebEventExt as _;

use super::{ClaimRelease, ComposerClaim, publish_measured};

/// A dock's hold on the shell's composer slot.
#[derive(Debug)]
pub struct ComposerSlot {
    token: Rc<Cell<u64>>,
    release: RefCell<Option<Rc<ClaimRelease>>>,
    element: RefCell<Option<Rc<MountedData>>>,
    #[cfg(target_arch = "wasm32")]
    observer: RefCell<
        Option<(
            web_sys::ResizeObserver,
            wasm_bindgen::closure::Closure<dyn FnMut()>,
        )>,
    >,
    /// What the dock's element reports, where there is no browser to ask.
    ///
    /// The real measurement is `getBoundingClientRect`, which cannot run
    /// off-browser, and "a dock publishes what it measures the moment it is
    /// handed the element" is a claim about ORDER that only something able to
    /// measure can see. It stands in for the NODE's report, never for the
    /// slot's own state — see [`ComposerSlot::dock_height`]. The field does not
    /// exist in a browser build.
    #[cfg(test)]
    measured: Cell<Option<f64>>,
}

impl Drop for ComposerSlot {
    /// Stop the observation before the callback is released.
    ///
    /// Releasing a `ResizeObserver` handle on its own does not stop it: the
    /// observed node can still deliver into a callback that has already been
    /// freed, which is the same use-after-free as the inline
    /// `Closure::wrap(…).as_ref().unchecked_ref()` this replaced. Fields drop
    /// after this returns, so the callback goes only once nothing can call it.
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some((observer, _)) = self.observer.borrow_mut().take() {
            observer.disconnect();
        }
    }
}

impl Default for ComposerSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl ComposerSlot {
    /// A dock that has not claimed the slot and has no element yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: Rc::new(Cell::new(0)),
            release: RefCell::new(None),
            element: RefCell::new(None),
            #[cfg(target_arch = "wasm32")]
            observer: RefCell::new(None),
            #[cfg(test)]
            measured: Cell::new(None),
        }
    }

    /// Measure `dock` from now on: its height goes to the shell on this call,
    /// and again whenever the drawer uncovers this dock and a new element is
    /// built for it.
    ///
    /// The element is stored BEFORE anything reads it. A slot that measured
    /// first and stored second cannot measure the element it was just handed at
    /// all, so the shell keeps a claim with no height under it — and the
    /// notification dock's lift, which is that height, collapses onto the
    /// composer until an observer delivery lands.
    pub fn attach(&self, dock: Rc<MountedData>) {
        *self.element.borrow_mut() = Some(dock);
        #[cfg(target_arch = "wasm32")]
        self.observe();
        self.measure();
    }

    /// Whether the dock is on screen, and therefore whether it holds the slot.
    pub fn set_on_screen(&self, on_screen: bool) {
        if self.release.borrow().is_some() == on_screen {
            return;
        }
        if on_screen {
            let claim = ComposerClaim::claim();
            self.token.set(claim.token);
            *self.release.borrow_mut() = Some(claim.release);
            self.measure();
        } else {
            self.token.set(0);
            // Dropping the release hands the slot back, to the newest survivor
            // when there is one and to nobody when there is not.
            self.release.borrow_mut().take();
        }
    }

    /// This dock's live identity, `0` while it holds nothing.
    #[must_use]
    pub fn token(&self) -> u64 {
        self.token.get()
    }

    /// Publish the dock's height under its live identity.
    fn measure(&self) {
        let Some(height) = self.dock_height() else {
            return;
        };
        publish_measured(self.token(), height);
    }

    /// What this dock's element reports right now, or `None` while the slot
    /// holds no element at all.
    ///
    /// THE ELEMENT IS THE PRECONDITION IN BOTH BUILDS. A browser can only
    /// measure a node it has been given, so a slot that has not been handed one
    /// has nothing to publish — and `0` is the one height the shell must never
    /// reserve from. The off-browser value stands in for what that node
    /// reports, NOT for the slot's own state, so a test double cannot hand this
    /// dock a height the browser could not have produced.
    fn dock_height(&self) -> Option<f64> {
        let has_element = self.element.borrow().is_some();
        if !has_element {
            return None;
        }
        #[cfg(test)]
        {
            self.measured.get()
        }
        #[cfg(not(test))]
        {
            measured_height(self.element.borrow().as_ref())
        }
    }

    /// Watch the dock's current element and report its height to the shell.
    ///
    /// The observer and its callback are stored TOGETHER in this dock, because
    /// a registration cannot outlive the thing it calls. A callback dropped at
    /// the end of this function leaves the observer calling a freed
    /// wasm-bindgen thunk: the browser throws on its first resize and every
    /// later measurement is lost, so the shell keeps the height the dock had at
    /// mount for as long as the page lives. The token is shared rather than
    /// copied for the same reason — the callback is released with the dock's
    /// observation, not with the render that built it.
    #[cfg(target_arch = "wasm32")]
    fn observe(&self) {
        use wasm_bindgen::JsCast as _;
        use wasm_bindgen::closure::Closure;

        // A dock the drawer uncovered is a NEW element, and an observer on the
        // old one would report a detached node's height for ever. Disconnect
        // before releasing, so the old callback is unreachable the moment it
        // is freed.
        if let Some((observer, _)) = self.observer.borrow_mut().take() {
            observer.disconnect();
        }
        let Some(element) = web_element(self.element.borrow().as_ref()) else {
            return;
        };
        let measured = element.clone();
        let token = Rc::clone(&self.token);
        let height: Rc<RefCell<f64>> = Rc::new(RefCell::new(f64::NAN));
        let seen = Rc::clone(&height);
        let callback = Closure::wrap(Box::new(move || {
            let next = measured.get_bounding_client_rect().height();
            // De-duplicated: a write per frame is a layout invalidation per
            // frame, and the shell only needs the value when it moved.
            if (next - *seen.borrow()).abs() < 0.5 {
                return;
            }
            *seen.borrow_mut() = next;
            publish_measured(token.get(), next);
        }) as Box<dyn FnMut()>);
        let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
            return;
        };
        observer.observe(element.as_ref());
        // Owned beside the observer that calls it, and freed only after that
        // observer is disconnected — see `Drop`. `mem::forget` would also be
        // memory-safe, and it is the shape this bug arrived in: a callback
        // nobody owns next to an observer everybody does, with nothing saying
        // which of the two is supposed to end the relationship.
        *self.observer.borrow_mut() = Some((observer, callback));
    }
}

/// A mounted dock's height in pixels, measured off its own node.
#[cfg(all(target_arch = "wasm32", not(test)))]
fn measured_height(dock: Option<&Rc<MountedData>>) -> Option<f64> {
    web_element(dock).map(|element| element.get_bounding_client_rect().height())
}

/// The mounted dock's DOM node, or `None` when it is not a browser element.
#[cfg(target_arch = "wasm32")]
fn web_element(dock: Option<&Rc<MountedData>>) -> Option<web_sys::Element> {
    let element = dock?.as_ref().try_as_web_event()?;
    Some(element.clone())
}

/// There is no node to measure off-browser, so the height is the one the caller
/// supplies — see [`ComposerSlot::dock_height`]. A test build does not define
/// this at all: its dock reports through that field, and a stub nothing calls
/// is only dead code wearing a signature.
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn measured_height(_dock: Option<&Rc<MountedData>>) -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::terminal_chrome::composer_geometry::published_geometry;

    /// A slot whose dock measures `height`, and the node a caller hands it.
    fn dock_measuring(height: f64) -> (Rc<ComposerSlot>, Rc<MountedData>) {
        let slot = Rc::new(ComposerSlot::new());
        slot.measured.set(Some(height));
        (slot, Rc::new(MountedData::new(())))
    }

    /// A dock that is on screen publishes its height the moment it is handed
    /// its element.
    ///
    /// The claim is taken first, because that is the order a mount runs in: a
    /// claim taken against an element the slot does not hold yet measures
    /// nothing, and the shell spends that frame reserving `0` for a composer
    /// that is already on screen.
    #[test]
    fn attaching_a_dock_publishes_its_height_on_the_same_call() {
        let (slot, dock) = dock_measuring(96.0);
        slot.set_on_screen(true);
        assert_eq!(
            published_geometry().height_px,
            0.0,
            "a claim with no element yet has nothing to measure"
        );

        slot.attach(dock);
        assert!(
            published_geometry().active,
            "an attached dock still holds the slot"
        );
        assert_eq!(
            published_geometry().height_px,
            96.0,
            "attach must publish what the dock measures without waiting for the observer"
        );
    }

    /// The drawer covers the dock and uncovers it without unmounting it, so the
    /// claim follows visibility in both directions and comes back with the
    /// height the same element measures.
    #[test]
    fn a_covered_dock_gives_the_slot_back_and_takes_it_again_at_its_measured_height() {
        let (slot, dock) = dock_measuring(96.0);
        slot.set_on_screen(true);
        slot.attach(dock);

        slot.set_on_screen(false);
        assert!(
            !published_geometry().active,
            "a dock the drawer covers holds nothing, and a detached node measures 0"
        );

        slot.set_on_screen(true);
        assert!(published_geometry().active);
        assert_eq!(
            published_geometry().height_px,
            96.0,
            "the dock is uncovered with the element it already measured"
        );
    }

    /// A visibility signal that resolves to the same answer must not re-claim:
    /// a second claim would take the slot from the first and leave the first
    /// holding a token the shell no longer answers to.
    #[test]
    fn repeated_visibility_keeps_one_claim() {
        let (slot, _dock) = dock_measuring(96.0);
        slot.set_on_screen(true);
        let claimed = slot.token();
        assert!(claimed > 0, "an on-screen dock holds a token");

        slot.set_on_screen(true);
        assert_eq!(
            slot.token(),
            claimed,
            "the same visibility must not claim a second time"
        );

        slot.set_on_screen(false);
        assert_eq!(slot.token(), 0, "a covered dock holds nothing");
    }
}

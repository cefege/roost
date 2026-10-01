//! One in-flight device open, shared by every caller that arrives while it runs.
//!
//! Split out of `super::audio_capture` because the sharing IS the contract, and
//! it is the one a browser hands out at most once: the tap that warms the device
//! and the recording that follows are the same open, and the second one must
//! not be a second charge against a browser that may never answer the first.
//! Ports the `warming` promise of `apps/web/src/voice/audioPcmCapture.ts`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What a caller hears when the shared open finishes: `None` once the device
/// has attached, or the caption the open failed with.
///
/// Both halves are load-bearing. A device that opened is not a recording — only
/// an attached device AND an open socket make one live — and an open that never
/// answers is a caption the operator has to read, not a line in a log nobody is
/// watching. A silent `None` for a dead open is what made a dead mic look live.
pub type OpenListener = Rc<dyn Fn(Option<String>)>;

/// The open every caller of `audio_capture::start_capture` joins while it runs.
#[derive(Default)]
pub struct OpenDevice {
    listeners: RefCell<Vec<OpenListener>>,
    settled: Cell<bool>,
    failure: RefCell<Option<String>>,
}

impl std::fmt::Debug for OpenDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenDevice")
            .field("listeners", &self.listeners.borrow().len())
            .field("settled", &self.settled.get())
            .finish()
    }
}

impl OpenDevice {
    /// Listen for the outcome, answering at once when the open has already
    /// finished — a caller that joins a settled open must not wait for a second
    /// verdict that will never come.
    pub(super) fn join(self: &Rc<Self>, listener: OpenListener) {
        if self.settled.get() {
            listener(self.failure.borrow().clone());
            return;
        }
        self.listeners.borrow_mut().push(listener);
    }

    /// Finish the open and tell every caller that joined it.
    pub(super) fn settle(self: &Rc<Self>, failure: Option<String>) {
        self.settled.set(true);
        *self.failure.borrow_mut() = failure;
        for listener in self.listeners.borrow_mut().drain(..) {
            listener(self.failure.borrow().clone());
        }
    }
}

/// The page's one in-flight open, and the decision of who is the one opening it.
///
/// Separate from [`OpenDevice`] because the sharing decision is a pure question
/// — is an open already running, or does this caller start one — and it must be
/// answerable without a browser. It is also the only place the in-flight open is
/// stored, so no caller can read the slot and re-enter it mid-decision.
#[derive(Debug, Default)]
pub struct OpenSlot {
    open: Option<Rc<OpenDevice>>,
}

impl OpenSlot {
    /// Join the open already running, or park a fresh one for this caller.
    ///
    /// The flag is whether THIS caller is the one that has to open the device.
    /// Exactly one caller per open is told yes, so the device is charged once.
    pub(super) fn join_or_start(&mut self) -> (Rc<OpenDevice>, bool) {
        match &self.open {
            Some(open) => (Rc::clone(open), false),
            None => {
                let open = Rc::new(OpenDevice::default());
                self.open = Some(Rc::clone(&open));
                (open, true)
            }
        }
    }

    /// Drop the in-flight open, so the next tap opens a fresh device instead of
    /// joining a promise nobody is going to settle.
    pub(super) fn release(&mut self) {
        self.open = None;
    }

    /// Whether the slot holds an open that has not answered yet.
    ///
    /// The two deadlines that watch a recording both ask "has the device
    /// arrived", and the open answers it better than either: it knows WHICH
    /// step stalled. A deadline that decides first replaces a caption naming
    /// the microphone with one that only says to tap again.
    pub(super) fn pending(&self) -> bool {
        self.open.as_ref().is_some_and(|open| !open.settled.get())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::OpenSlot;

    fn recorder() -> Rc<Cell<usize>> {
        Rc::new(Cell::new(0))
    }

    #[test]
    fn one_caller_opens_the_device_and_the_next_joins_that_same_open() {
        let mut slot = OpenSlot::default();
        let (first, first_opens) = slot.join_or_start();
        let (second, second_opens) = slot.join_or_start();
        assert!(
            first_opens,
            "the first caller is the one that opens the device"
        );
        assert!(
            !second_opens,
            "a caller arriving mid-open must not open a second"
        );
        assert!(Rc::ptr_eq(&first, &second), "both callers share one open");
    }

    #[test]
    fn a_released_slot_lets_the_next_tap_open_a_fresh_device() {
        let mut slot = OpenSlot::default();
        let (first, _) = slot.join_or_start();
        slot.release();
        let (second, opens) = slot.join_or_start();
        assert!(opens);
        assert!(
            !Rc::ptr_eq(&first, &second),
            "a released open is not rejoined"
        );
    }

    #[test]
    fn every_caller_joined_to_one_open_hears_the_same_verdict() {
        let mut slot = OpenSlot::default();
        let (open, _) = slot.join_or_start();
        let heard = recorder();
        for _ in 0..3 {
            let tally = Rc::clone(&heard);
            open.join(Rc::new(move |failure: Option<String>| {
                tally.set(tally.get() + 1);
                assert_eq!(failure.as_deref(), Some("the device stayed shut"));
            }));
        }
        open.settle(Some("the device stayed shut".to_owned()));
        assert_eq!(heard.get(), 3, "one verdict reaches every joined caller");
    }

    #[test]
    fn a_caller_joining_after_the_verdict_is_answered_at_once() {
        let mut slot = OpenSlot::default();
        let (open, _) = slot.join_or_start();
        open.settle(None);
        let late = recorder();
        let tally = Rc::clone(&late);
        open.join(Rc::new(move |failure: Option<String>| {
            assert!(failure.is_none());
            tally.set(tally.get() + 1);
        }));
        assert_eq!(late.get(), 1, "a late joiner waits on no second verdict");
    }

    #[test]
    fn asking_the_slot_while_an_open_is_still_in_flight_does_not_re_enter_it() {
        // The decision reads the slot and then hands the open out; a caller that
        // asks again from inside that window must see the same open, not panic
        // on a borrow the slot is still holding.
        let mut slot = OpenSlot::default();
        let (open, _) = slot.join_or_start();
        let (again, opens_again) = slot.join_or_start();
        assert!(!opens_again);
        assert!(Rc::ptr_eq(&open, &again));
    }

    #[test]
    fn the_slot_reports_an_open_until_it_has_answered() {
        // The deadlines that watch a recording read this to stay off a verdict
        // the open is still going to produce, so a settled open must read as
        // answered and a released slot as nothing at all.
        let mut slot = OpenSlot::default();
        assert!(!slot.pending(), "a slot that never opened is not pending");

        let (open, _) = slot.join_or_start();
        assert!(slot.pending(), "an open nobody has answered is pending");

        open.settle(None);
        assert!(!slot.pending(), "a settled open is no longer pending");

        slot.release();
        assert!(!slot.pending(), "a released slot holds no open to wait for");
    }
}

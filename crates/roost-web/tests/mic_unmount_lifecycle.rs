//! A mic that goes away hands the page's microphone back.
//!
//! THE REGRESSION. A recording ends in two ways in this page: the engine
//! settles, or the composer that owns it stops existing. The second one is not
//! hypothetical — the mobile drawer UNMOUNTS the portaled composer dock rather
//! than deactivating it, so the composer that was mid-recording, mid-hypothesis
//! and holding the page's voice slot disappears without the machine ever being
//! told. What it leaves behind is worse than a recording nobody stopped:
//!
//!   - the slot is still held. `VoiceSlot::claim` is a compare-and-set on a
//!     token, and a composer's token is minted by its own mount, so the composer
//!     that mounts next carries a DIFFERENT one and is refused. Every recording
//!     after the first is dead, which is the whole reported class.
//!   - the hypothesis the composer painted over its draft stays in the draft,
//!     and a hypothesis is by definition words the recognizer has not committed
//!     to. The next send ships them to the PTY.
//!
//! What is pinned here is the observable the browser would show: after the
//! composer that claimed the page's microphone is unmounted, the page's slot is
//! free again and the owner was told the mic is no longer dictating. The
//! recording's own words are the state machine's contract and are pinned in
//! `voice/state/tests.rs`; what was missing is the component ever asking for it.
//!
//! The tap here is `voice::shell_controls::toggle`, which is what a pad binding
//! and the mic button both end up calling, so the test drives the real door
//! rather than a private one. A host build with no browser has no engine, so the
//! start is refused — which is exactly the shape that needs the guard, because
//! the slot is claimed BEFORE the refusal is decided.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_web::components::mobile_voice_input::MobileVoiceInput;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;
use roost_web::voice::ownership::with_slot;
use roost_web::voice::shell_controls;
use roost_web::voice::state::LiveTranscript;

/// The session whose composer the mic belongs to.
const SESSION_ID: &str = "00000000-0000-4000-8000-00000000000b";

thread_local! {
    /// Whether the composer is mounted, read by the root and written by the test.
    static MOUNTED: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
    /// Every `active` the mic reported to its owner, in order.
    static TOLD_ACTIVE: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
}

/// The mic on its own, as a composer mounts it: `active`, so nothing finishes
/// the recording but the mic itself, and with a pump because the component
/// reads the coordinator's stored engine configuration through one.
fn mic_root() -> Element {
    use_context_provider(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-mic-teardown"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    let mounted = use_signal(|| true);
    MOUNTED.with(|slot| *slot.borrow_mut() = Some(mounted));
    let told = EventHandler::new(|active: bool| {
        TOLD_ACTIVE.with(|seen| seen.borrow_mut().push(active));
    });
    rsx! {
        if mounted() {
            MobileVoiceInput {
                owner_id: SESSION_ID.to_owned(),
                active: true,
                on_active_change: told,
                on_transcript: EventHandler::new(|_words: String| {}),
                on_live_transcript: EventHandler::new(|_update: Option<LiveTranscript>| {}),
                on_discard: EventHandler::new(|_thrown_away: ()| {}),
            }
        }
    }
}

/// The tree the root published, with the handle the test unmounts through.
struct Mounted {
    dom: VirtualDom,
    mounted: Signal<bool>,
}

impl Mounted {
    fn open() -> Self {
        let mut dom = VirtualDom::new(mic_root);
        dom.rebuild(&mut NoOpMutations);
        dom.process_events();
        let mounted = MOUNTED.with(|slot| slot.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always publishes the \
             mount flag; an empty slot means the render pass never happened",
        );
        Self { dom, mounted }
    }

    /// Run the mic's own activation, as a tap or a pad binding does.
    fn toggle_mic(&mut self) {
        self.dom.in_runtime(shell_controls::toggle);
        self.dom.process_events();
    }

    /// Unmount the composer, and run the teardown that goes with it.
    fn unmount_composer(&mut self) {
        self.dom.in_runtime(|| self.mounted.set(false));
        self.dom.process_events();
        self.dom.render_immediate(&mut NoOpMutations);
        self.dom.process_events();
    }
}

/// The claims the page's voice slot has heard, in order.
fn claims() -> Vec<bool> {
    TOLD_ACTIVE.with(|seen| seen.borrow().clone())
}

#[test]
fn a_composer_that_goes_away_leaves_the_page_microphone_free() {
    TOLD_ACTIVE.with(|seen| seen.borrow_mut().clear());
    let mut mic = Mounted::open();
    assert!(
        !with_slot(|slot| slot.is_claimed()),
        "a composer that has not been asked to record holds nothing, or the test below is \
         measuring a slot that was already taken"
    );

    // The mic's activation claims the page's slot before it decides whether an
    // engine can answer, so a start that is refused still has to hand it back.
    mic.toggle_mic();
    assert!(
        with_slot(|slot| slot.is_claimed()),
        "the tap did not claim the page's microphone; without that claim there is nothing for \
         the teardown below to hand back and the guard proves nothing"
    );

    mic.unmount_composer();

    assert!(
        !with_slot(|slot| slot.is_claimed()),
        "a composer that was unmounted still holds the page's voice slot. Its token dies with \
         it, and the composer that mounts next is refused by the claim — so every recording \
         after this one is dead until the page reloads."
    );
}

#[test]
fn a_composer_that_goes_away_tells_its_owner_the_mic_is_idle() {
    TOLD_ACTIVE.with(|seen| seen.borrow_mut().clear());
    let mut mic = Mounted::open();

    mic.unmount_composer();

    assert_eq!(
        claims(),
        vec![false],
        "a composer that went away while dictating must report that it is not dictating any \
         more; anything else leaves the shell's Enter key and send button gated open, or a live \
         recording with nothing able to stop it"
    );
}

//! The acknowledgement a visible, focused view of a coding-agent session earns.
//!
//! Mounts the REAL component over a real pump, because the decision and the
//! dispatch it makes are two halves of one rule, and a test that stopped at the
//! predicate would pass with the dispatch missing. Mirrors
//! `crates/roost-web/src/components/notifications/agent_notifications/agent_attention.rs`;
//! the title rules are `agent_attention_count.rs`.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_attention_support;

use std::cell::RefCell;
use std::rc::Rc;

use agent_attention_support::{OTHER, VIEWED, agent_status, seed_blocked_status};
use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_protocol::wire::{AgentRuntimeState, AgentStatus};
use roost_web::components::notifications::agent_notifications::agent_attention::{
    AgentAttention, unacknowledged_view,
};
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;
use roost_web::router_state::RouterContext;

/// How many render-then-event rounds the harness gives the effect. The bound
/// stops a cycle from hanging the test rather than from failing it.
const SETTLE_ROUNDS: usize = 4;

thread_local! {
    /// The pump the root built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

/// The route the shell renders, and the core behind it.
struct Harness {
    dom: VirtualDom,
    pump: Pump,
}

impl Harness {
    fn settle(&mut self) {
        for _ in 0..SETTLE_ROUNDS {
            self.dom.render_immediate(&mut NoOpMutations);
            self.dom.process_events();
        }
    }

    fn core(&self) -> Rc<RefCell<ClientCore>> {
        self.pump.core()
    }

    fn acknowledged(&self, status: &AgentStatus) -> i64 {
        let core = self.core();
        let core = core.borrow();
        core.store().agent_seen.acknowledged_revision(status)
    }

    fn revision(&self) -> u64 {
        let core = self.core();
        let core = core.borrow();
        core.store().revision()
    }
}

/// The root the attention owner is mounted under: a pump over the seeded core,
/// and the router the owner reads its viewed session from.
fn root() -> Element {
    let mut core = ClientCore::in_memory("agent-attention");
    seed_blocked_status(&mut core);
    use_context_provider(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(core)),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    use_context_provider(|| RouterContext {
        path: Signal::new(format!("/s/{VIEWED}")),
        navigate: EventHandler::new(|_next: String| {}),
    });
    rsx! { AgentAttention {} }
}

fn mounted() -> Harness {
    let mut dom = VirtualDom::new(root);
    dom.rebuild_in_place();
    Harness {
        pump: BUILT
            .with(|built| built.borrow_mut().take())
            .expect("the root runs during the first rebuild and always builds a pump"),
        dom,
    }
}

/// THE RULE. A visible, focused view of a session with an unacknowledged revision
/// spends that acknowledgement — and spends it through the ledger, so the rows
/// that read it change with no second persistence path in the tree.
#[test]
fn a_visible_focused_view_spends_the_acknowledgement_it_owes() {
    let mut harness = mounted();
    let status = agent_status(VIEWED, AgentRuntimeState::Blocked, 2, 0);
    assert_eq!(
        harness.acknowledged(&status),
        -1,
        "an identified occupant nothing has acknowledged floors below every \
         revision, so its newest report is genuinely unseen"
    );

    harness.settle();

    assert_eq!(
        harness.acknowledged(&status),
        2,
        "looking at a blocked agent is the acknowledgement; the row must stop \
         reading as owed once the reader is on it"
    );

    // Nothing left to spend, so a further pass must not move the store: an owner
    // that re-dispatched on its own acknowledgement would repaint forever.
    let settled = harness.revision();
    harness.settle();
    assert_eq!(
        harness.revision(),
        settled,
        "an acknowledgement at or below what is already recorded moves nothing, \
         so the effect cannot feed itself"
    );
}

/// The gate is visible AND focused, not visible alone. A Roost window parked on a
/// second monitor is being read, not looked at, and acknowledging there would
/// clear a blocked agent nobody has seen.
#[test]
fn an_unattended_page_owes_nothing_and_a_route_off_the_session_owes_nothing() {
    let mut core = ClientCore::in_memory("agent-attention-gate");
    seed_blocked_status(&mut core);
    let store = core.store();
    let route = format!("/s/{VIEWED}");

    assert_eq!(
        unacknowledged_view(true, store, &ExactWorkerPaths, &route),
        Some(VIEWED.to_owned()),
        "the session on screen with an unacknowledged revision is the one owed"
    );
    assert_eq!(
        unacknowledged_view(false, store, &ExactWorkerPaths, &route),
        None,
        "hidden or unfocused, the reader is not looking at this tab and nothing \
         may be marked seen for them"
    );
    assert_eq!(
        unacknowledged_view(true, store, &ExactWorkerPaths, "/"),
        None,
        "the workbench is not a session: a view of no agent acknowledges no agent"
    );
    assert_eq!(
        unacknowledged_view(true, store, &ExactWorkerPaths, &format!("/s/{OTHER}")),
        None,
        "a session with no status row has no occupant this profile could owe"
    );
}

/// Once the ledger records the revision, the same view owes nothing further —
/// which is what makes the badge count fall rather than stick.
#[test]
fn an_acknowledged_revision_is_not_owed_again() {
    let mut core = ClientCore::in_memory("agent-attention-ack");
    let status = seed_blocked_status(&mut core);
    assert_eq!(
        unacknowledged_view(
            true,
            core.store(),
            &ExactWorkerPaths,
            &format!("/s/{VIEWED}")
        ),
        Some(VIEWED.to_owned())
    );

    core.store_mut().agent_seen.mark_seen(&status);

    assert_eq!(
        unacknowledged_view(
            true,
            core.store(),
            &ExactWorkerPaths,
            &format!("/s/{VIEWED}")
        ),
        None
    );
}

//! The ring a hovered notification paints on the surface its View action lands
//! on: the pane tab that already shows the target, and the sidebar folder row
//! that stands in for it when nothing does.
//!
//! Native, and split into the two halves it is made of: the REPAINT, which is
//! only observable as a render count over a real signal, and the CHOICE of
//! surface, which is a pure function of a store. Mounting a row that peeked at
//! the hold is what the first half pins, and it is why the read subscribes.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::Store;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::toasts::{ToastId, ToastSource};
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, Worker, WorkerFp,
    WorkerOs,
};
use roost_web::components::notifications::notify_target::{
    NotifyTarget, RingHold, folder_ring_attribute, open_tab_session_ids, ring_attribute,
};

/// The machine every fixture session belongs to.
const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The session the probe row paints for.
const PROBED: &str = "30000000-0000-4000-8000-000000000001";
/// A second terminal sharing `/tmp` with `PROBED`: a pane tab represents it.
const SIBLING: &str = "30000000-0000-4000-8000-000000000002";
/// A terminal in another folder, so nothing on screen represents it.
const REMOTE: &str = "30000000-0000-4000-8000-000000000003";

/// How many render-then-event rounds the harness gives a write. The bound stops
/// a cycle from hanging the test rather than from failing it.
const SETTLE_ROUNDS: usize = 4;

thread_local! {
    /// The ring the root provided, handed back out of the render pass.
    static RING: RefCell<Option<NotifyTarget>> = const { RefCell::new(None) };
    /// How many times the probe rendered.
    static RENDERS: Cell<u32> = const { Cell::new(0) };
    /// What the probe painted on its last render, before any render.
    static PAINTED: Cell<Option<Option<&'static str>>> = const { Cell::new(None) };
}

/// The root `VirtualDom::new` wants: the ring in context, and the row that
/// paints for one session.
fn root() -> Element {
    let target = NotifyTarget::provide();
    RING.with(|ring| *ring.borrow_mut() = Some(target));
    rsx! { RingProbe { session_id: PROBED.to_owned() } }
}

/// A surface that paints the ring, reading the hold during render exactly as the
/// pane tab and the three sidebar row surfaces do.
#[component]
fn RingProbe(session_id: String) -> Element {
    let target = try_use_context::<NotifyTarget>().expect("the root provides the ring");
    PAINTED.set(Some(ring_attribute(&target, &session_id)));
    RENDERS.set(RENDERS.get() + 1);
    rsx! { div { class: "df-row", "probe" } }
}

/// Run the work a write queued, the way the browser runs it: the scopes the
/// write marked dirty re-render, then the dom's effects run.
fn settle(dom: &mut VirtualDom) {
    for _ in 0..SETTLE_ROUNDS {
        dom.render_immediate(&mut NoOpMutations);
        dom.process_events();
    }
}

/// What the probe painted on its last render.
fn painted() -> Option<&'static str> {
    PAINTED.get().expect("the probe rendered at least once")
}

/// Drive the ring from outside the render, the way a card's pointer handlers do.
fn ring_with(dom: &mut VirtualDom, hold: impl FnOnce(&mut NotifyTarget)) {
    dom.in_runtime(|| {
        RING.with(|ring| {
            hold(
                ring.borrow_mut()
                    .as_mut()
                    .expect("the root provided the ring"),
            );
        });
    });
    settle(dom);
}

/// One card, distinguished from another by the toast it owns.
fn card(key: &str) -> ToastId {
    ToastId::new(ToastSource::Host { name: "agent" }, key)
}

/// The probe, mounted and settled with nothing ringing.
fn mounted() -> VirtualDom {
    PAINTED.set(None);
    RENDERS.set(0);
    let mut dom = VirtualDom::new(root);
    dom.rebuild_in_place();
    settle(&mut dom);
    dom
}

/// THE REGRESSION. A hover arrives as a pointer handler, and a handler schedules
/// no repaint of its own: the target rows kept the frame they first drew, so a
/// card the reader was holding had no ring anywhere on the page and its View
/// action named a place the page had not marked.
#[test]
fn a_ring_signal_change_reaches_the_rows_that_paint_it() {
    let mut dom = mounted();
    let before = RENDERS.get();
    assert!(before > 0, "the probe renders on mount");
    assert_eq!(painted(), None, "nothing has been hovered yet");

    ring_with(&mut dom, |ring| ring.ring(&card(PROBED), Some(PROBED)));

    assert!(
        RENDERS.get() > before,
        "a ring written from a pointer handler schedules no repaint, so a row \
         that reads the hold without subscribing never learns about it"
    );
    assert_eq!(painted(), Some("true"));

    ring_with(&mut dom, |ring| ring.clear(&card(PROBED)));
    assert_eq!(painted(), None, "the leave takes the ring back down");
}

/// A pointer that jumps from one card straight to the next makes the first card
/// emit its leave AFTER the second has already rung. A clear that did not name
/// its holder would take the second card's ring down with it.
#[test]
fn a_late_leave_only_takes_down_the_card_that_took_the_ring() {
    let mut dom = mounted();
    let first = card("first");
    let second = card("second");
    ring_with(&mut dom, |ring| ring.ring(&first, Some(PROBED)));
    ring_with(&mut dom, |ring| ring.ring(&second, Some(PROBED)));
    assert_eq!(painted(), Some("true"));

    ring_with(&mut dom, |ring| ring.clear(&first));
    assert_eq!(
        painted(),
        Some("true"),
        "the first card's leave arrived after the second card rang, and it must \
         not take the second card's ring down with it"
    );

    ring_with(&mut dom, |ring| ring.clear(&second));
    assert_eq!(painted(), None);
}

/// One agent rings once. The folder row stands in for a target no pane tab
/// shows; a target the on-screen strip already carries is answered by that tab.
#[test]
fn a_folder_row_is_not_ringed_while_a_pane_tab_already_shows_the_session() {
    let core = seeded_core();
    let store = core.store();
    let tmp = bucket_key(store, PROBED);
    let root = bucket_key(store, REMOTE);
    let on_screen = open_tab_session_ids(store, &ExactWorkerPaths, &format!("/s/{SIBLING}"));
    assert_eq!(
        on_screen,
        vec![PROBED.to_owned(), SIBLING.to_owned()],
        "the deck paints the route's folder, and every live terminal in it holds \
         a tab"
    );

    let ringed_tab = RingHold {
        holder: None,
        session_id: Some(SIBLING.to_owned()),
    };
    assert_eq!(
        folder_ring_attribute(&ringed_tab, store, &ExactWorkerPaths, &on_screen, &tmp),
        None,
        "the tab strip already rings this session, so its folder row would ring \
         one agent twice on two surfaces the reader cannot act on together"
    );

    let off_screen = RingHold {
        holder: None,
        session_id: Some(REMOTE.to_owned()),
    };
    assert_eq!(
        folder_ring_attribute(&off_screen, store, &ExactWorkerPaths, &on_screen, &root),
        Some("true"),
        "nothing on screen represents a terminal in another folder, so its row \
         is the only surface left to point at"
    );
    assert_eq!(
        folder_ring_attribute(&off_screen, store, &ExactWorkerPaths, &on_screen, &tmp),
        None,
        "one folder row carries the ring; the rows beside it do not"
    );
}

/// A target the store no longer holds has no row to ring, which is the honest
/// answer rather than a ring on whichever folder key happens to come first.
#[test]
fn a_target_the_store_no_longer_holds_rings_no_folder() {
    let core = seeded_core();
    let store = core.store();
    let gone = RingHold {
        holder: None,
        session_id: Some("30000000-0000-4000-8000-0000000000ff".to_owned()),
    };
    assert_eq!(
        folder_ring_attribute(
            &gone,
            store,
            &ExactWorkerPaths,
            &[],
            &bucket_key(store, PROBED)
        ),
        None
    );
}

/// The bucket a fixture terminal's row lives in, read through the store's own
/// selector rather than spelled out, so the test pins the ring rule and not the
/// shape of a key.
fn bucket_key(store: &Store, session_id: &str) -> String {
    let session = session_by_id(store, session_id).expect("the fixture holds this session");
    session_folder_key(store, &ExactWorkerPaths, session)
}

fn session(id: &str, cwd: &str) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).expect("a uuid"),
        worker_fp: WorkerFp::try_from(WORKER_FP.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(1_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: cwd.to_owned(),
        spawn_cwd: Some(cwd.to_owned()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: 1,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

/// A store with two terminals in `/tmp` and one in `/`, so one session is on the
/// on-screen tab strip and one is off it.
fn seeded_core() -> ClientCore {
    let mut core = ClientCore::in_memory("notify-target-ring");
    let store = core.store_mut();
    let mut map = SessionMap::new();
    for (id, cwd) in [PROBED, SIBLING, REMOTE]
        .into_iter()
        .zip(["/tmp", "/tmp", "/"])
    {
        let row = session(id, cwd);
        map.insert(row.id.clone(), row);
    }
    store.workers.insert(
        WORKER_FP.to_owned(),
        Worker {
            fp: WorkerFp::try_from(WORKER_FP.to_owned()).expect("a fingerprint"),
            label: "fixture".to_owned(),
            os: WorkerOs::Linux,
            host_identity: None,
            git_sha: None,
            host_metrics: None,
            registered_at_ms: 1,
            last_seen_ms: 1,
            reachable_addr: None,
            keeper_runtime: None,
            terminal_core_capacity: None,
        },
    );
    store.sessions.apply_snapshot(map);
    core
}

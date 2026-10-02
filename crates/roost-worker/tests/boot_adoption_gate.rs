//! Adoption and respawn against a REAL keeper, the way a restarted worker meets
//! them: v2 `session-resume.ts` rebuilds a record around a surviving PTY from
//! the keeper's ordered history (head + base geometry), and v2
//! `session-respawn.ts` retires a held record by killing its PTY once the
//! `respawned` event is durable. Drives `SessionManager::adopt_survivor` and
//! `respawn_session` over `KeeperFixture` and a real SQLite journal.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;

use roost_host::supported_host_platform;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_protocol::wire::brand::SessionId;
use roost_worker::browser_commands::search::MAX_ACTIVE_SEARCHES;
use roost_worker::event_store::DurableEventKind;
use roost_worker::event_store::Journal;
use roost_worker::keeper_pool::KeeperPool;
use roost_worker::runtime::session_stack::{self, SessionStack};
use roost_worker::session::respawn_replace::RespawnRequest;
use roost_worker::session::resume::AdoptionRequest;
use roost_worker::session::spawn::ClaimsOnFailure;

use keeper_pool_support::{KeeperFixture, channel, opened, session, sh_spec, wait_until};

/// ONE KEEPER AT A TIME IN THIS BINARY.
///
/// Each test starts a real keeper daemon on its own socket and opens a real
/// PTY child, and the keeper client bounds its own spawn acknowledgement
/// (`roost_keeper::client::SPAWN_ACK_TIMEOUT`). Two of those running
/// concurrently on a machine that is also building four tracks starves a
/// daemon past that bound, and the failure reads as a product defect — a
/// refused spawn — when it is the fixture competing with itself. This is the
/// same lock, and for the same reason, as `keeper_survivor_adoption.rs`'s —
/// an ASYNC one here, because these bodies await while holding it, and a
/// `std` guard held across an `.await` is what clippy refuses.
static FIXTURE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The fixture lock, held for a whole test body.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    FIXTURE.lock().await
}

/// The fingerprint the worker's own identity would carry at boot.
const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000f00d";

/// A directory this test owns, removed by the test body when it is done.
fn scratch(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("roost-boot-gate-{label}-{unique}"));
    std::fs::create_dir_all(&root).expect("the fixture can make its own directory");
    root
}

/// The session layer a RESTARTED worker has: a new pool over the same keeper,
/// and a durable outbox of its own.
///
/// The outbox is a real SQLite journal rather than a stub because the stack
/// owns the claim an adoption reserves for the survivor's close, and a stack
/// that could not reserve one would refuse for the wrong reason and hide what
/// this test is about.
async fn restarted_stack(pool: Arc<KeeperPool>, root: &std::path::Path) -> SessionStack {
    let outbox = Arc::new(
        Journal::open(&root.join("session-events.sqlite"))
            .await
            .expect("a fresh directory holds a fresh journal"),
    );
    session_stack::build(
        roost_protocol::wire::brand::WorkerFp::try_from(FINGERPRINT)
            .expect("the fixture fingerprint is 64 lowercase hex"),
        pool,
        outbox,
        root,
        root,
        FINGERPRINT.to_owned(),
        "boot-adoption-epoch",
        &roost_worker::agents::environment::AgentReportSite {
            data_dir: root.to_path_buf(),
            configured: None,
        },
    )
    .expect("this host can build a session layer")
}

/// Whether a process this pid names is still running.
///
/// `/proc`, and not a signal: a signal proves nothing about a zombie, and the
/// distinction between "the keeper reaped it" and "the worker killed it" is the
/// whole assertion.
fn running(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

/// v2 `resume`: a restarted worker adopts the survivor from the keeper's
/// ordered history — the record's head is the keeper's byte head, the PTY and
/// its pid are untouched, and the session is live under the coordinator's id.
#[tokio::test]
async fn a_restarted_worker_adopts_its_survivor_from_the_keepers_history() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive().await;
    let root = scratch("adopt");
    let (binding, record) = session("survivor-with-history");
    // The original worker's pool stays alive until its child has printed: the
    // pool's dispatch loop is what delivers output, and it ends with the pool
    // (v2 keeper-survivor-continuity.test.ts:274-288 destroys the original
    // socket only after the marker was observed).
    let original = fixture.pool();
    let spawned = opened(
        original.spawn(
            channel(1),
            &sh_spec(&["-c", "printf HELLO-SURVIVOR; sleep 30"], &[]),
            100,
            40,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );
    record.printed("HELLO-SURVIVOR");
    drop(original);

    let restarted = fixture.pool();
    let stack = restarted_stack(Arc::clone(&restarted), &root).await;
    // Session ids are UUIDs on the wire (v2 session-spawn.ts:53 mints
    // `asSessionId(randomUUID())`).
    let session_id =
        SessionId::try_from("00000000-0000-4000-8000-0000000000a1".to_owned()).unwrap();
    let request = AdoptionRequest {
        session_id: session_id.clone(),
        channel_id: channel(1),
        folder: root.display().to_string(),
        shell_spec: sh_spec(&["-c", "true"], &[]),
        close_reservation: stack
            .manager
            .reserve(DurableEventKind::Closed)
            .await
            .unwrap(),
    };
    let adopted = stack
        .manager
        .adopt_survivor(&request)
        .await
        .expect("a keeper that reports its head and base geometry is adoptable");

    assert!(
        adopted.head_seq >= "HELLO-SURVIVOR".len() as u64,
        "the record's head is the keeper's byte head: {adopted:?}"
    );
    assert!(running(spawned.pid), "an adoption never touches the PTY");
    assert_eq!(
        stack.table.channel_of(&session_id),
        Some(spawned.channel_id)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// v2 `session-respawn.ts:169-175`: respawning a session this worker still
/// holds opens a new channel under the SAME session id and, once the
/// `respawned` is durable, kills the PTY it replaced — no orphan survives it.
///
/// MULTI-THREADED because the replaced channel's exit reaches the session
/// layer as soon as the keeper reports it, and the close that follows is a
/// task holding the pool. On a current-thread runtime nothing runs that task
/// once the body stops awaiting, so the pool's connection stays open and the
/// fixture's drop waits forever for the in-process keeper to finish serving it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn respawning_a_held_session_kills_the_pty_it_replaces() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive().await;
    let root = scratch("respawn");
    let (binding, record) = session("held-then-respawned");
    let spawned = opened(
        fixture.pool().spawn(
            channel(1),
            &sh_spec(&["-c", "sleep 30"], &[]),
            80,
            24,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );
    record.printed("");
    let restarted = fixture.pool();
    let stack = restarted_stack(Arc::clone(&restarted), &root).await;
    stack
        .manager
        .advance_past_keeper()
        .expect("the keeper answers its list");
    let session_id =
        SessionId::try_from("00000000-0000-4000-8000-0000000000a2".to_owned()).unwrap();
    let request = AdoptionRequest {
        session_id: session_id.clone(),
        channel_id: channel(1),
        folder: root.display().to_string(),
        shell_spec: sh_spec(&["-c", "sleep 30"], &[]),
        close_reservation: stack
            .manager
            .reserve(DurableEventKind::Closed)
            .await
            .unwrap(),
    };
    stack
        .manager
        .adopt_survivor(&request)
        .await
        .expect("the survivor is adopted");

    let respawn = RespawnRequest {
        session_id: session_id.clone(),
        cwd: root.display().to_string(),
        shell_spec: None,
        cols: None,
        rows: None,
    };
    let event = stack
        .manager
        .reserve(DurableEventKind::State)
        .await
        .unwrap();
    let close = stack
        .manager
        .reserve(DurableEventKind::Closed)
        .await
        .unwrap();
    let respawned = stack
        .manager
        .respawn_session(respawn, event, close, ClaimsOnFailure::Release)
        .await
        .expect("a held session is respawned in place");

    assert_ne!(respawned.channel_id, spawned.channel_id, "a new channel");
    assert_eq!(
        stack.table.channel_of(&session_id),
        Some(respawned.channel_id),
        "under the same session id"
    );
    wait_until(|| !running(spawned.pid), "the replaced PTY to be killed");
    let _ = std::fs::remove_dir_all(&root);
}

/// THE F6 GUARD, AND IT GOES THROUGH `SessionStack::deps` OR IT PROVES
/// NOTHING.
///
/// The first version of this test built its own `Arc<Mutex<Searches>>` and
/// cloned it, which would have passed whether or not F6 was fixed — the second
/// `Deps` was made by the TEST, not by the product, so the test was measuring a
/// copy it had built itself. That is the same tautology as F5's first guard,
/// and the only defence is to call the real accessor and let the product build
/// both sides.
///
/// `Searches::admit` bounds concurrent scrollback searches at
/// `MAX_ACTIVE_SEARCHES`. That bound is only a bound if there is ONE ledger:
/// two ledgers mean two callers each believe they hold all eight, and a machine
/// runs sixteen — which is the number the bound exists to prevent.
#[tokio::test]
async fn two_deps_from_one_session_stack_share_one_admission_ledger() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive().await;
    let root = scratch("admission-ledger");

    let platform = supported_host_platform().expect("this host runs v3");
    let pool = fixture.pool();
    let stack = restarted_stack(Arc::clone(&pool), &root).await;

    // BOTH Deps come from the product, by the `pub` accessor the finding is
    // about. Nothing here constructs a `Searches`.
    let first = stack.deps(platform);
    let second = stack.deps(platform);

    // A DISTINCT owner key per search, and that is load-bearing rather than
    // incidental: `Searches::admit` REPLACES the running search under the same
    // owner_key rather than competing with it for the bound. One shared key
    // would leave the running set at one entry, the bound would never be
    // reached, and this guard would have passed against a ledger that admitted
    // anything — a guard measuring its own fixture, which is the F5 tautology
    // again in a new costume.
    // THE `who` PREFIX IS THE WHOLE POINT, and getting it wrong twice is what
    // this comment is for. `Searches::admit` REPLACES the running search under
    // the same `owner_key` rather than competing with it for the bound, so:
    // one shared key leaves the running set at one entry and the bound is
    // never reached; and the SAME keys on both Deps replace the first Deps'
    // eight with the second's eight and still admit. The second set has to
    // carry keys the first does not, or this measures replacement rather than
    // the shared bound.
    let admitted = |deps: &roost_worker::browser_commands::Deps, who: &str, index: usize| {
        deps.searches
            .lock()
            .expect("held")
            .admit(
                &format!("{who}-owner-{index}"),
                &format!("search-{index}"),
                false,
            )
            .is_ok()
    };

    for index in 0..MAX_ACTIVE_SEARCHES {
        assert!(
            admitted(&first, "first", index),
            "the first Deps is refused at {index} of {MAX_ACTIVE_SEARCHES}, \
             before the bound is reached"
        );
    }
    let past_the_bound = (0..MAX_ACTIVE_SEARCHES)
        .filter(|index| admitted(&second, "second", *index))
        .count();
    assert_eq!(
        past_the_bound, 0,
        "a second Deps admitted {past_the_bound} searches past the \
         {MAX_ACTIVE_SEARCHES}-slot bound. The bound is per LEDGER, and two \
         Deps from one SessionStack share one — if this fails, `deps()` builds \
         a fresh `Searches` per call and the bound is eight PER Deps, which is \
         not what it claims to be."
    );

    std::fs::remove_dir_all(&root).ok();
}

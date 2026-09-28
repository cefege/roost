//! A boot must not reach into a live terminal it cannot rebuild a record
//! around.
//!
//! `KeeperPool::channel_history` is a hard refusal against every keeper this
//! build speaks to: the socket reports neither the history head a replay needs
//! nor the base geometry the oldest retained record was produced at. So
//! `SessionManager::adopt_survivor` cannot complete — and it has already done
//! the part that MUTATES before it discovers that, calling `deliver_into` at
//! `resume.rs:201` to rebind the channel's keeper output into a staged
//! binding, then failing on the history at `:204`.
//!
//! WHAT IS MEASURED, AND IT IS NOT THE OBVIOUS STORY. With the gate disabled
//! (mutation row M-W1, run and recorded in the part-2 commit) the child is
//! still ALIVE: the `channel_history` refusal returns through a plain
//! `map_err` and never calls `abandon`. The damage is silent orphaning — the
//! terminal's output is staged and never parsed, `Staging::stage_output`
//! discards the held events once `RESUME_STAGE_CAP_BYTES` is passed, no record
//! is installed, no browser can attach, and the reserved close claim leaks.
//! `abandon` — and so `kill_channel` — runs on `adopted_record` failure, on a
//! failed insert and on staging overflow, all three of which become reachable
//! the moment W-K's missing `GetHistory`/`GetTerminalState` frames land. The
//! same boot would then kill every terminal instead of orphaning it.
//!
//! So the gate's job is to keep the adoption — and the reattach it performs —
//! from starting at all, and the assertions here are chosen to be things a
//! test can OBSERVE rather than things it has to take on trust: a process that
//! is still running, a channel the keeper still holds, and counters that say
//! the adoption was never attempted.
//!
//! This drives it against a REAL keeper on a real socket with a REAL PTY
//! child. A fake would report whatever the code under test read; only a real
//! child on a real keeper can be observed alive.
//!
//! Depends on `keeper_pool_support` for the fixture and on nothing else.

mod keeper_pool_support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_proto::Session;
use roost_worker::event_store::Journal;
use roost_worker::keeper_pool::KeeperPool;
use roost_worker::runtime::adoption;
use roost_worker::runtime::session_stack::{self, SessionStack};
use roost_worker::session::resume::KeeperChannels;

use keeper_pool_support::{KeeperFixture, channel, opened, session, sh_spec};

/// ONE KEEPER AT A TIME IN THIS BINARY.
///
/// Each test starts a real keeper daemon on its own socket and opens a real
/// PTY child, and the keeper client bounds its own spawn acknowledgement
/// (`roost_keeper::client::SPAWN_ACK_TIMEOUT`). Two of those running
/// concurrently on a machine that is also building four tracks starves a
/// daemon past that bound, and the failure reads as a product defect — a
/// refused spawn — when it is the fixture competing with itself. This is the
/// same lock, and for the same reason, as `keeper_survivor_adoption.rs`'s.
static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The fixture lock, held for a whole test body.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    FIXTURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The fingerprint the worker's own identity would carry at boot.
const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000f00d";

/// What the adoption request carries as the socket the record was adopted over.
///
/// PROVENANCE ONLY, and stated rather than read: `AdoptionRequest::socket_path`
/// is recorded on the record and is not used to reach anything, and
/// `KeeperFixture` does not expose its socket. Adding an accessor to a support
/// module this slice does not own would be a wider change than the test needs.
const ADOPTED_OVER: &str = "<the fixture keeper's socket, which the fixture does not expose>";

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

/// THE GATE. A keeper that cannot describe a survivor's history must leave that
/// survivor alone — held, not killed, not adopted, not abandoned, and above
/// all NOT REBOUND — and the boot must continue past it.
///
/// The assertions are in the order they matter, and each fails for a different
/// reason:
///
///  1. `/proc/<pid>` is still there. The child is a real process; this is the
///     only assertion that is about a terminal rather than about code, and it
///     is the one a future edit that moves the probe behind the adoption
///     would break — through `abandon` calling `kill_channel` the day the
///     keeper can answer a history request.
///  2. the keeper's own list still holds the channel with the SAME pid, so the
///     terminal was neither reaped nor replaced.
///  3. the counters say the adoption was never ATTEMPTED. This is what
///     actually discriminates today, and the mutation row confirms it: with
///     the probe neutered, the first two assertions still pass and this one
///     fails, because the damage with the gate off is the REATTACH — the pool
///     rebinding a terminal's output into a staged binding behind a record
///     that was never installed — not a kill.
#[tokio::test]
async fn a_survivor_the_keeper_cannot_describe_is_left_running_rather_than_killed() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let root = scratch("held");

    // The last worker opened this terminal. Its own pool goes out of scope
    // when the function ends; the keeper outlives it on purpose, and the PTY
    // survives with the keeper as its only owner.
    let (binding, record) = session("survivor-under-a-refusing-keeper");
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
    assert!(running(spawned.pid), "the child is running before the boot");

    // The refusing keeper is the shipped one: `channel_history` returns a
    // `KeeperFault` naming NO_REPORTED_HEAD and NO_REPORTED_BASE_GEOMETRY
    // against a live, healthy, holding-a-PTY keeper. Asserted rather than
    // assumed, so this test cannot silently stop being about a refusing keeper.
    let restarted = fixture.pool();
    let refusal = KeeperChannels::channel_history(restarted.as_ref(), spawned.channel_id)
        .expect_err("this keeper reports no head, so no history is assembled");
    assert_eq!(
        refusal.operation, "channel_history",
        "the fault this gate exists to intercept comes from the history request itself"
    );

    let stack = restarted_stack(Arc::clone(&restarted), &root).await;

    // The coordinator's own open-session row for this channel, which is what
    // makes the survivor ADOPTABLE in principle. Without it the adoption would
    // be skipped for a different reason and would prove nothing.
    let open = [Session {
        id: "session-under-a-refusing-keeper".to_owned(),
        channel: u32::from(spawned.channel_id),
        cwd: root.display().to_string(),
        status: "open".to_owned(),
        ..Default::default()
    }];

    let outcome = adoption::adopt_survivors(
        &stack,
        &restarted,
        &[spawned.channel_id],
        &open,
        ADOPTED_OVER,
    )
    .await;

    // THE SURVIVAL ASSERTION COMES FIRST, before the counters. It is the one
    // that matters; the counters are the diagnosis. The other way round, a test
    // failing on `unreplayable == 1` would say nothing about whether the
    // terminal lived, and that is the question the gate exists to answer.
    assert!(
        running(spawned.pid),
        "the survivor was KILLED by a boot that only meant to decline it: \
         `adopt_survivor` calls `abandon` — and so `kill_channel` — on three of \
         its six refusal paths, and all three become reachable the moment the \
         keeper can answer a history request. The gate is what keeps this boot \
         out of that code entirely"
    );
    let live = KeeperChannels::live_channels(restarted.as_ref())
        .expect("the keeper still answers its list");
    let survivor = live
        .iter()
        .find(|held| held.channel_id == spawned.channel_id)
        .expect("a terminal the gate declined to adopt is still held by the keeper");
    assert_eq!(
        survivor.pid, spawned.pid,
        "and it is the same process, not a replacement"
    );

    assert_eq!(
        outcome.unreplayable, 1,
        "the survivor was declined by the probe, not offered to the adoption and refused by it"
    );
    assert_eq!(
        outcome.refused, 0,
        "the adoption was never attempted, so nothing was killed by an abandonment"
    );
    assert_eq!(outcome.adopted, 0);

    std::fs::remove_dir_all(&root).ok();
}

/// A boot that declines EVERY survivor must still return — the gate is not an
/// error path, and a refused adoption is not a boot failure.
///
/// This is the second half of the property. The first test proves a survivor
/// survives; this one proves that declining it costs the operator nothing but
/// an `info` line: the function returns its counters rather than an `Err`, and
/// an empty adoption set is a claim, not a failure.
#[tokio::test]
async fn a_boot_that_declines_every_survivor_still_completes() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let root = scratch("completes");

    let restarted = fixture.pool();
    let stack = restarted_stack(Arc::clone(&restarted), &root).await;

    // Channels the keeper does not hold at all, which is the shape a machine
    // reaches with nothing to adopt: the gate must answer for each and the
    // caller must get a value back.
    let outcome = adoption::adopt_survivors(
        &stack,
        &restarted,
        &[7, 8, 9],
        &[Session {
            id: "absent".to_owned(),
            channel: 7,
            cwd: root.display().to_string(),
            ..Default::default()
        }],
        ADOPTED_OVER,
    )
    .await;

    assert_eq!(
        outcome,
        adoption::Adopted {
            adopted: 0,
            unreplayable: 3,
            refused: 0,
            unknown_to_coordinator: 0,
            unreservable: 0,
        },
        "three undescribable survivors, three declines, and no other outcome to report"
    );

    std::fs::remove_dir_all(&root).ok();
}

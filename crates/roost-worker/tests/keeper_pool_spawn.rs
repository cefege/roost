#![cfg(unix)]
//! What a spawn through the keeper pool guarantees, asserted on a real keeper:
//! a real socket, a real PTY, and a real child whose own output is the evidence.
//! The channel table's own guarantees live in `keeper_pool_channels.rs`.
//! Depends on `keeper_pool_support` for the fixture — nothing here else.

mod keeper_pool_support;

use std::sync::Arc;
use std::time::Instant;

use keeper_pool_support::{
    KeeperFixture, channel, child_environment, child_text, opened, refused, session, sh_spec,
    wait_until,
};
use roost_worker::keeper_pool::PoolError;
use roost_worker::session::sinks::ChannelBinding;
use roost_worker::session::spawn::ShellSpawner;
use roost_worker::shell_spec::KEEPER_CONTROL_ENV_PREFIX;

/// THE SECURITY PROPERTY, END TO END. `is_keeper_control_key` is already
/// tested as a predicate; what matters is downstream of it — a worker that
/// leaks a credential hands every command a user types the ability to speak to
/// the keeper as this worker, which is every terminal on the machine. So this
/// reads the environment out of a SPAWNED CHILD rather than asserting on the
/// predicate, and it would catch a strip that is missing, applied to the wrong
/// copy of the spec, or case-sensitive.
#[test]
fn a_keeper_control_capability_never_reaches_a_pty() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    // Every spelling a case-SENSITIVE check would wave through, plus the bare
    // prefix. A mixed-case credential in a PTY is a credential.
    let capabilities: Vec<(String, String)> = [
        KEEPER_CONTROL_ENV_PREFIX.to_string(),
        "ROOST_KEEPER_ENDPOINT".to_string(),
        "roost_keeper_capability".to_string(),
        "Roost_Keeper_Capability_Path".to_string(),
        "ROOST_keeper_endpoint_kind".to_string(),
    ]
    .into_iter()
    .map(|name| (name, "keeper-capability-value".to_string()))
    .collect();
    let mut spec = sh_spec(&["-c", "env"], &[("ROOST_SESSION_ID", "kept")]);
    spec.env.extend(capabilities);
    let (binding, record) = session("env-probe");

    let spawned = opened(
        pool.spawn(
            channel(1),
            &spec,
            80,
            24,
            Arc::new(binding) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );
    let seen = record.settled();

    assert_eq!(seen.error, None, "the channel failed instead of running");
    let environment = child_environment(&seen);
    // The child really did print an environment, so what follows is about THIS
    // environment and not about a channel that produced nothing at all.
    assert!(
        environment
            .iter()
            .any(|(key, value)| key == "ROOST_SESSION_ID" && value == "kept"),
        "the child's environment was not printed: {environment:?}"
    );
    let leaked: Vec<&String> = environment
        .iter()
        .map(|(key, _)| key)
        .filter(|key| {
            key.to_ascii_uppercase()
                .starts_with(KEEPER_CONTROL_ENV_PREFIX)
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "a keeper control credential reached the PTY: {leaked:?}"
    );
    assert!(spawned.pid > 0, "a real child was opened");
}

/// CORRELATION IS WHAT MAKES A CONCURRENT POOL SAFE. Eight sessions race to
/// spawn; each must be answered with its own channel and its own child's bytes.
/// A pool that answered by arrival order, or that routed output to one shared
/// binding, hands two of these the same marker — and a session painting another
/// session's shell is not a bug anyone would find from a log line.
#[test]
fn concurrent_spawns_are_answered_one_for_one() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || {
                let marker = format!("marker-{index}");
                let (binding, record) = session("concurrent-spawn");
                let spawned = opened(
                    pool.spawn(
                        channel(index + 1),
                        &sh_spec(&["-c", &format!("echo {marker}; sleep 5")], &[]),
                        80,
                        24,
                        Arc::new(binding) as Arc<dyn ChannelBinding>,
                    ),
                    "every concurrent spawn is answered",
                );
                // The text is read BEFORE `marker` moves into the tuple: tuple
                // elements evaluate in order, so `(spawned, marker, …&marker)`
                // borrows a value the previous element already consumed.
                let text = record.printed(&marker);
                (spawned, marker, text)
            })
        })
        .collect();

    let mut channels: Vec<u16> = Vec::new();
    for thread in threads {
        let (spawned, marker, text) = match thread.join() {
            Ok(answered) => answered,
            Err(_) => panic!("a spawn thread panicked before it could answer"),
        };
        assert!(
            text.contains(&marker),
            "channel {} heard {text:?} instead of {marker}",
            spawned.channel_id
        );
        assert!(spawned.pid > 0, "an acknowledged spawn has a process");
        channels.push(spawned.channel_id);
    }
    channels.sort_unstable();
    channels.dedup();
    assert_eq!(channels.len(), 8, "every spawn got a channel of its own");
    assert_eq!(
        pool.live_bindings().len(),
        8,
        "every answered spawn is announced"
    );
}

/// THE FIRST BYTES AND THE EXIT RIDE RIGHT BEHIND THE ACK. A child that prints
/// and exits at once has both on the wire before its spawning thread runs
/// again, so a pool that acknowledged the channel only after releasing the
/// connection let the dispatcher route them to a channel its table did not yet
/// answer for: a session that showed nothing, or showed nothing and never
/// ended. Sixteen at once, because the dispatcher wins that race only
/// sometimes.
#[test]
fn a_child_that_prints_and_exits_at_once_is_heard_and_ends() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let threads: Vec<_> = (0..16)
        .map(|index| {
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || {
                let marker = format!("brief-{index}");
                let (binding, record) = session("brief-child");
                opened(
                    pool.spawn(
                        channel(index + 1),
                        &sh_spec(&["-c", &format!("echo {marker}; exit 3")], &[]),
                        80,
                        24,
                        Arc::new(binding) as Arc<dyn ChannelBinding>,
                    ),
                    "every brief spawn is answered",
                );
                (marker, record.settled())
            })
        })
        .collect();

    for thread in threads {
        let (marker, seen) = match thread.join() {
            Ok(settled) => settled,
            Err(_) => panic!("a brief child's session never settled"),
        };
        assert!(
            child_text(&seen).contains(&marker),
            "{marker} was never delivered: {seen:?}"
        );
        assert_eq!(seen.exit, Some(Some(3)), "{marker} ended wrongly: {seen:?}");
    }
}

/// A REFUSAL MUST LEAVE NOTHING BEHIND. A spawn that failed has no PTY, so a
/// binding left registered would route a LATER frame for that id into a session
/// that never existed, and would announce a channel nothing owns to a keeper
/// that will then refuse to reap it.
#[test]
fn a_refused_spawn_leaves_no_tracked_channel() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let (refused_binding, _) = session("refused");
    let mut spec = sh_spec(&["-c", "exit 0"], &[]);
    // A folder that does not exist: the keeper checks it BEFORE it forks, so
    // this is a keeper-side refusal rather than a worker-side one.
    spec.cwd = "/roost-keeper-pool/no-such-folder".to_string();

    let err = refused(
        pool.spawn(
            channel(1),
            &spec,
            80,
            24,
            Arc::new(refused_binding) as Arc<dyn ChannelBinding>,
        ),
        "the keeper refuses a folder it cannot enter",
    );
    assert!(matches!(err, PoolError::Keeper(_)), "{err}");
    assert!(
        pool.live_bindings().is_empty(),
        "a refused spawn is not a channel: {:?}",
        pool.live_bindings()
    );
    assert!(
        pool.spawning_channels().is_empty(),
        "a refused spawn is not in flight: {:?}",
        pool.spawning_channels()
    );

    // The next spawn proves the refused id no longer resolves to the refusal: an
    // id that still reached the failed binding would send this child's bytes
    // into a session that never existed. It is a DIFFERENT id now, and that is
    // the caller's allocator's business — the pool used to bump its own counter
    // here, which is the second allocator this change removed.
    let (binding, record) = session("after-refusal");
    opened(
        pool.spawn(
            channel(2),
            &sh_spec(&["-c", "echo alive; sleep 5"], &[]),
            80,
            24,
            Arc::new(binding) as Arc<dyn ChannelBinding>,
        ),
        "the pool still works after a refusal",
    );
    // `printed` returning IS the isolation proof: this channel received its own
    // child's bytes. A binding left behind by the refused spawn would have taken
    // them instead, and the session that never existed would have stayed silent.
    record.printed("alive");
    let announced: Vec<u16> = pool
        .live_bindings()
        .into_iter()
        .map(|binding| binding.channel_id)
        .collect();
    assert_eq!(
        announced.len(),
        1,
        "only the channel that really opened is tracked: {announced:?}"
    );
}

/// THE COLLISION THIS POOL REFUSES, END TO END ON A REAL KEEPER. A keeper
/// outlives the worker that spawned it, so a fresh worker that starts its
/// counter at one can name a channel an orphaned PTY is still holding, and the
/// spawn is answered `channel_id in use` — a new terminal that fails for a
/// reason naming the wire rather than the collision. This used to be the
/// pool's own allocator's problem, and the pool no longer holds one, so the
/// refusal is all that is left standing between a caller's mistake and a
/// terminal that never opens.
#[test]
fn a_spawn_named_a_channel_the_keeper_still_holds_is_refused() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let (binding, record) = session("collides");
    let spawned = opened(
        pool.spawn(
            channel(88),
            &sh_spec(&["-c", "sleep 30"], &[]),
            80,
            24,
            Arc::new(binding) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );
    record.printed("");

    // The pool has now LEARNED the id, because it asked the keeper what it
    // holds. Everything the keeper reports is what a fresh worker must avoid.
    let held = opened(
        pool.keeper_channels(),
        "the keeper reports the channel it holds",
    );
    assert_eq!(
        held.iter().map(|c| c.channel_id).collect::<Vec<_>>(),
        vec![spawned.channel_id],
        "the keeper holds exactly the channel that was opened"
    );

    // The colliding id itself, and one BELOW the mark: a counter that handed
    // out 88 has passed 1..=87, so those are spent too even though the keeper
    // holds none of them.
    for colliding in [88u16, 3] {
        let (other, _) = session("collides-too");
        let err = refused(
            pool.spawn(
                channel(colliding),
                &sh_spec(&["-c", "sleep 30"], &[]),
                80,
                24,
                Arc::new(other) as Arc<dyn ChannelBinding>,
            ),
            "a spawn must not be handed a channel the keeper is using",
        );
        assert!(
            matches!(err, PoolError::ChannelIdTaken { channel_id, highest: 88 } if channel_id == colliding),
            "the refusal names the id and the mark: {err}"
        );
    }

    // The refusal happened BEFORE anything was written, so the running PTY is
    // untouched and no binding was left registered for a channel that never
    // opened. That is what makes the refusal safe to retry from.
    assert_eq!(
        opened(pool.keeper_channels(), "the keeper still answers")
            .iter()
            .map(|c| c.channel_id)
            .collect::<Vec<_>>(),
        vec![spawned.channel_id],
        "a refused colliding spawn left nothing behind"
    );
    opened(
        pool.input(spawned.channel_id, b""),
        "the channel the collision would have hit is still running",
    );
}

/// A KILL IS PROVEN BY THE KEEPER REAPING, NOT BY A FRAME. The daemon owes
/// nothing for a `KillChild` — `roost_keeper::client::kill` writes it and
/// returns — so the only observable outcome is the channel leaving
/// `list_channels`. That is also why the seam returns nothing: a caller that
/// waited for an answer here would sit out a whole query timeout on every
/// close, and this is the close that runs when a spawn has already failed.
#[test]
fn a_killed_channel_leaves_the_keeper_and_nothing_else_is_asked_of_it() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let (survivor, survivor_record) = session("outlives-the-kill");
    let doomed = opened(
        pool.spawn(
            channel(1),
            &sh_spec(&["-c", "sleep 30"], &[]),
            80,
            24,
            Arc::new(survivor) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );
    let (victim, _victim_record) = session("killed");
    let killed = opened(
        pool.spawn(
            channel(2),
            &sh_spec(&["-c", "sleep 30"], &[]),
            80,
            24,
            Arc::new(victim) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a second real PTY",
    );
    survivor_record.printed("");

    let before = Instant::now();
    ShellSpawner::kill_channel(pool.as_ref(), channel(killed.channel_id));
    let elapsed = before.elapsed();

    // The reaping is the keeper's, and it is asynchronous: what this asserts is
    // that the channel goes, not that a frame came back.
    wait_until(
        || matches!(pool.keeper_channels(), Ok(list) if !list.iter().any(|c| c.channel_id == killed.channel_id)),
        "the keeper to reap the killed channel",
    );

    // The call itself did not wait for anything. A generous bound rather than
    // a tight one: the property under test is that this returns without the
    // keeper's answer, and a query timeout here is seconds.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "a kill must not wait for a frame the daemon never sends: {elapsed:?}"
    );

    // And the kill took exactly the one channel. A kill that ended the keeper,
    // or a sibling, would have taken a terminal somebody is using.
    let remaining = opened(pool.keeper_channels(), "the keeper still answers");
    assert_eq!(
        remaining.iter().map(|c| c.channel_id).collect::<Vec<_>>(),
        vec![doomed.channel_id],
        "only the channel that was killed is gone"
    );
    opened(
        pool.input(doomed.channel_id, b""),
        "the surviving channel still answers",
    );
    survivor_record.printed("");
}

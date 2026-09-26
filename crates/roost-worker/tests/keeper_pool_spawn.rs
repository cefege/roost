//! What a spawn through the keeper pool guarantees, asserted on a real keeper:
//! a real socket, a real PTY, and a real child whose own output is the evidence.
//! The channel table's own guarantees live in `keeper_pool_channels.rs`.
//! Depends on `keeper_pool_support` for the fixture — nothing here else.

mod keeper_pool_support;

use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, child_environment, opened, refused, session, sh_spec};
use roost_worker::keeper_pool::PoolError;
use roost_worker::session::sinks::ChannelBinding;
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
        pool.spawn(&spec, 80, 24, Arc::new(binding) as Arc<dyn ChannelBinding>),
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
                        &sh_spec(&["-c", &format!("echo {marker}; sleep 5")], &[]),
                        80,
                        Arc::new(binding) as Arc<dyn ChannelBinding>,
                    ),
                    "every concurrent spawn is answered",
                );
                (spawned, marker, record.printed(&marker))
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
    // into a session that never existed.
    let (binding, record) = session("after-refusal");
    opened(
        pool.spawn(
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

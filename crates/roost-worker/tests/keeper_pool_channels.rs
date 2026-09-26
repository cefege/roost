//! What the pool's channel table guarantees once the keeper is answering: a
//! channel that ended stops being announced, and a keeper that is gone tells
//! every channel exactly once. Driven through real PTYs on a real keeper,
//! because both properties are about frames that actually arrive.
//! Depends on `keeper_pool_support` for the fixture — nothing here else.

mod keeper_pool_support;

use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, opened, session, sh_spec, wait_until};
use roost_worker::keeper_pool::PoolError;
use roost_worker::session::sinks::ChannelBinding;

/// A BINDING ANNOUNCED TO THE KEEPER IS ONE IT WILL REFUSE TO REAP. So an
/// ended channel must leave the announced set the moment its exit frame lands,
/// or a later process that adopts this keeper inherits a channel list holding a
/// process nothing owns.
#[test]
fn an_exited_channel_is_omitted_from_the_live_bindings() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let (doomed, doomed_record) = session("exits-7");
    let (alive, _) = session("stays");

    let ended = opened(
        pool.spawn(
            &sh_spec(&["-c", "exit 7"], &[]),
            80,
            24,
            Arc::new(doomed) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );
    let staying = opened(
        pool.spawn(
            &sh_spec(&["-c", "sleep 5"], &[]),
            80,
            24,
            Arc::new(alive) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );

    let seen = doomed_record.settled();
    assert_eq!(seen.exit, Some(Some(7)), "the child's own code is reported");
    assert_eq!(seen.output, Vec::<u8>::new(), "exit 7 printed nothing");
    // The claim is the table's decision and the notice is the caller's, so this
    // read races the dispatch thread unless the test waits for the claim.
    wait_until(
        || pool.has_exited(ended.channel_id),
        "the pool to record the exit",
    );

    let announced: Vec<u16> = pool
        .live_bindings()
        .into_iter()
        .map(|binding| binding.channel_id)
        .collect();
    assert_eq!(
        announced,
        vec![staying.channel_id],
        "only the live channel is announced to the keeper"
    );
    opened(
        pool.input(staying.channel_id, b""),
        "the live channel still answers",
    );
}

/// A LOST KEEPER IS NOT A LOST TERMINAL. The keeper outlives its connection, so
/// every PTY it was driving may still be running in it; the sessions are told
/// the connection is gone, not that their shells ended. And they are told ONCE,
/// because a channel that ends twice is a close the caller sees twice.
#[test]
fn a_lost_keeper_tells_every_channel_exactly_once() {
    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let (first, first_record) = session("first");
    let (second, second_record) = session("second");
    opened(
        pool.spawn(
            &sh_spec(&["-c", "sleep 5"], &[]),
            80,
            24,
            Arc::new(first) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );
    opened(
        pool.spawn(
            &sh_spec(&["-c", "sleep 5"], &[]),
            80,
            24,
            Arc::new(second) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY",
    );

    pool.keeper_lost("the keeper process ended".to_string());
    pool.keeper_lost("the keeper process ended".to_string());

    for record in [&first_record, &second_record] {
        let seen = record.settled();
        assert_eq!(seen.exit, None, "the PTY may still be running");
        assert_eq!(
            seen.error.as_deref(),
            Some("the keeper process ended"),
            "every channel is told once, with the reason"
        );
    }
    assert!(
        pool.live_bindings().is_empty(),
        "a lost keeper owns nothing here"
    );
    let err = refused_after_loss(&pool);
    assert!(matches!(err, PoolError::Disconnected(_)), "{err}");
    assert!(
        !pool.is_connected(),
        "the pool does not still claim a keeper"
    );
}

/// A request after the loss must fail rather than be written into a socket
/// nobody reads: a silent success leaves a session typing into a void.
fn refused_after_loss(pool: &roost_worker::keeper_pool::KeeperPool) -> PoolError {
    match pool.input(1, b"x") {
        Ok(()) => panic!("a request after the loss was accepted"),
        Err(err) => err,
    }
}

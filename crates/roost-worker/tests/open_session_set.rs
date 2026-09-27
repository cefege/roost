//! What a coordinator that did not answer is, and what a boot then does about a
//! keeper that is holding somebody's terminal.
//!
//! THE PROPERTY, IN TWO HALVES THAT ONLY MAKE SENSE TOGETHER. The first half is
//! that an unanswered coordinator is `None` and not `Some(0)`. The second is
//! that `None` leaves a survivor alone. A test with only the first would pass
//! with the composition root's `unwrap_or_else` collapsed into `unwrap_or(0)`,
//! because `read_open_session_count`'s own `Err` is not what the boot reads — it
//! reads what this decision turns that `Err` into. The two halves are the same
//! decision, so they are one test.
//!
//! WHY THE DECISION IS CALLABLE AT ALL. It was an `unwrap_or_else` two lines
//! wide inside `runtime::serve_until`, which is why it shipped untested. It is
//! now `runtime::reconcile::open_sessions_or_unknown`, and that is what this
//! file calls — not a re-implementation of it, which would test nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "retire_support/mod.rs"]
mod retire_support;

use std::path::Path;

use connectrpc::client::{ClientConfig, HttpClient};
use retire_support::{FakeKeeper, boot, platform};
use roost_keeper::frames::ChannelBinding;
use roost_proto::CoordinatorServiceClient;

use roost_worker::runtime::keeper_boot::{self, KeeperBootOutcome};
use roost_worker::runtime::reconcile::open_sessions_or_unknown;

/// A suffix no two fixtures in this binary can share, because the tests run in
/// parallel and a `Drop` that removed a directory another test was still using
/// looks exactly like a missing file.
static FIXTURE_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn unique() -> u32 {
    FIXTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// ONE DIR, removed when the value goes out of scope. The same shape
/// `boot_env_support` gives every other test that resolves a boot.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-open-session-{name}-{}", unique()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch root is creatable");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// THE PAIR, in one test, because either half alone is satisfiable by a
/// decision that is wrong.
///
/// AND A CORRECTION WORTH READING, because I got it wrong first and the code
/// said so. I expected a probed survivor HOLDING a terminal to come back
/// `Held`, because that is what "leave it alone" sounds like. It comes back
/// `Adopted`, and it would come back `Adopted` no matter what the open-session
/// set said — because `boot_keeper::admit` returns `StartFresh` only when
/// `channels.is_empty()` (`boot_keeper.rs:156`), so a keeper that PROVED it
/// holds a terminal is `Adopt` or `Blocked` and the coordinator's count is
/// never read. The count gates exactly one branch: replacing a keeper that
/// holds nothing (`keeper_boot.rs:161-170`).
///
/// So the pair is asserted where the two readings actually diverge — a keeper
/// holding nothing, `None` versus `Some(0)` — and the holding case is asserted
/// separately for what it really guarantees, which is that a terminal is
/// disturbed by neither reading.
#[tokio::test]
async fn an_unread_open_session_set_is_not_a_licence_to_replace_anything() {
    // ---- FIRST HALF: the read really does fail, and becomes `None`. ----
    let refusing = refusing_client();
    let read =
        roost_worker::runtime::reconcile::read_open_session_count(&refusing, "fp-under-test")
            .await
            .expect_err("a coordinator that is not there cannot answer");
    let open_sessions = open_sessions_or_unknown(Err(read));
    assert_eq!(
        open_sessions, None,
        "a coordinator that did not answer is not a coordinator with nothing open"
    );

    // A coordinator that DID answer is passed through untouched, including a
    // genuine zero — which is a claim, and the only claim that authorises a
    // replacement. Collapsing this branch is the same bug as the one above.
    assert_eq!(open_sessions_or_unknown(Ok(Some(0))), Some(0));
    assert_eq!(open_sessions_or_unknown(Ok(Some(3))), Some(3));

    // ---- SECOND HALF: `None` HOLDS a replaceable keeper, `Some(0)` replaces it.
    // This is the pair that fails if the composition root's `unwrap_or_else`
    // ever becomes an `unwrap_or(0)`, and it is the ONLY place that collapse is
    // observable — so it is the only place a test for it can live.
    let scratch = Scratch::new("unread");
    let first = boot(scratch.path(), platform());
    let keeper = FakeKeeper::start(&first, platform()).await;

    let unread = keeper_boot::ensure_keeper(&first, open_sessions, &first.log_dir)
        .await
        .expect("a probed keeper is admitted one way or another");
    assert!(
        matches!(unread, KeeperBootOutcome::Held { .. }),
        "an unread open-session set is not a licence to replace a keeper: {unread:?}"
    );
    // The same keeper, still listening, is proof the first call replaced
    // nothing: a replacement shuts the old keeper down.
    drop(keeper);

    let scratch = Scratch::new("answered");
    let second = boot(scratch.path(), platform());
    let _keeper = FakeKeeper::start(&second, platform()).await;
    let answered = keeper_boot::ensure_keeper(&second, Some(0), &second.log_dir)
        .await
        .expect("a keeper the coordinator vouches for is replaceable");
    assert!(
        matches!(answered, KeeperBootOutcome::StartedFresh { .. }),
        "and a coordinator that says nothing is open IS a licence: the two \
         readings must differ here or the first assertion proves nothing \
         about the count: {answered:?}"
    );
}

/// THE OTHER HALF OF THE SAME PROPERTY, and the one the session brief named: a
/// survivor with a live terminal behind it. It is ADOPTED, not held, and not
/// because the open-session set was read — the count is not consulted on this
/// path at all, which is what makes the guarantee stronger than the one above:
/// a terminal is safe from BOTH an unread set and a coordinator that vouches
/// for replacing everything, because the keeper itself proved it holds the
/// channel.
#[tokio::test]
async fn a_survivor_holding_a_terminal_is_adopted_by_either_reading() {
    let held = vec![ChannelBinding {
        channel_id: 1,
        pid: 4242,
    }];
    for (name, open_sessions) in [("unread", None), ("answered", Some(9))] {
        let scratch = Scratch::new(name);
        let boot = boot(scratch.path(), platform());
        let _keeper = FakeKeeper::start_holding(&boot, platform(), held.clone()).await;
        let outcome = keeper_boot::ensure_keeper(&boot, open_sessions, &boot.log_dir)
            .await
            .expect("a probed keeper is admitted one way or another");
        assert!(
            matches!(outcome, KeeperBootOutcome::Adopted { .. }),
            "{name}: a keeper that proved it holds channel 1 is adopted, and the \
             open-session set is not what decides it: {outcome:?}"
        );
    }
}

/// A Connect client pointed at a port nothing is listening on, built exactly
/// as `bootstrap_redeem::activation::coordinator_client` builds the real one.
///
/// The same `HttpClient` and `ClientConfig`, because a first half that used a
/// different transport would be testing a different failure. That function is
/// `pub(crate)` and so is not callable from an integration test, which is why
/// this repeats three lines rather than importing it — and why the repetition
/// is the thing to watch if that signature ever changes.
fn refusing_client() -> CoordinatorServiceClient<HttpClient> {
    let uri: axum::http::Uri = "http://127.0.0.1:1".parse().expect("a literal URI parses");
    CoordinatorServiceClient::new(HttpClient::plaintext(), ClientConfig::new(uri))
}

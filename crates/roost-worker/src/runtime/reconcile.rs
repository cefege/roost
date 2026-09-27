//! Reading the coordinator's open-session set, which is the one fact the keeper
//! survivor decision cannot proceed without. Called by the boot sequence once,
//! between the link dialling and the keeper being admitted.
//!
//! WHY IT IS A SEPARATE CALL AND NOT A LINK FRAME. v2 reads it with a Connect
//! unary during boot (`boot-session-reconcile.ts:96` calls `sessionsList`), not
//! over the worker socket, and for the same reason this does: the decision it
//! feeds is a decision about whether to DESTROY something, and a fact read over
//! a link that is still establishing itself is a fact the link can also be
//! retrying. A unary either answers or does not.
//!
//! WHY THE WHOLE SET AND NOT A COUNT. [`crate::runtime::keeper_boot::decide`]
//! takes a count, and the count is all the decision needs, but the set is what
//! is fetched and the count is derived here so the two cannot disagree about
//! what was asked for. v2 derived `coordinatorOpenSessionIds` from the same
//! response it admitted against.
//!
//! AN UNANSWERED COORDINATOR IS `None`, NOT ZERO. Zero says "this coordinator
//! has no open sessions", and a keeper survivor holding channels would then be
//! replaced on the strength of a coordinator that never replied. `None` says
//! "nobody has read it", and `decide` treats that as do-not-replace.

use anyhow::Context as _;
use connectrpc::client::ClientTransport;
// `http_body` by way of `connectrpc`, which is the path the sibling
// `bootstrap_redeem` module already uses for the same trait bound: two paths
// to one trait is a second version of it in the lockfile.
use connectrpc::http_body;
use roost_proto::{CoordinatorServiceClient, SessionsListRequest};

/// The open-session count, or `None` while nobody has read it.
pub type OpenSessionCount = Option<usize>;

/// Ask the coordinator which sessions it still lists as open on this worker.
///
/// `None` is returned by the CALLER for a coordinator that did not answer, and
/// the caller treats that as "unknown" — the direction that cannot end a
/// terminal. This function's own failure is an error rather than a `None`,
/// because the caller has to be able to say which of the two happened.
pub async fn read_open_session_count<T>(
    client: &CoordinatorServiceClient<T>,
    worker_fp: &str,
) -> anyhow::Result<OpenSessionCount>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    let request = SessionsListRequest {
        worker_fp: Some(worker_fp.to_owned()),
        // "open" is the default, and it is STATED rather than inherited: the
        // whole point of the call is that the set is the OPEN one, and a
        // coordinator that later changes its default would otherwise silently
        // widen this to include closed sessions — which is a set that says a
        // closed session is open, and the survivor decision acts on it.
        status: Some("open".to_owned()),
        sync_socket_id: None,
        ..Default::default()
    };
    let response = client
        .sessions_list(request)
        .await
        .context("the coordinator did not report its open-session set")?;
    let count = response.view().sessions.len();
    tracing::info!(
        %worker_fp,
        open_sessions = count,
        "the coordinator's open-session set is in hand, and the keeper survivor \
         decision may proceed on a read fact rather than an assumed-empty one"
    );
    Ok(Some(count))
}

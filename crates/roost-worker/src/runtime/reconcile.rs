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

/// The coordinator's open-session count, or `None` while nobody has read it.
pub type OpenSessionCount = Option<usize>;

/// The coordinator's own row for a session it still lists as open here.
///
/// THE ROW, NOT THE COUNT, and that is the whole reason this type exists. A
/// survivor's adoption needs this session's id, its folder, the coordinator's
/// own stream generation and the trace every event about it has carried — and
/// every one of those is a FIELD of this row rather than something the worker
/// may invent. Reading the count and throwing the rows away left the adoption
/// with no identity to adopt INTO, and the fields it then made up described a
/// session wearing another session's channel.
pub type OpenSession = roost_proto::Session;

/// Ask the coordinator which sessions it still lists as open on this worker.
///
/// `None` is returned by the CALLER for a coordinator that did not answer, and
/// the caller treats that as "unknown" — the direction that cannot end a
/// terminal. This function's own failure is an error rather than a `None`,
/// because the caller has to be able to say which of the two happened.
pub async fn read_open_sessions<T>(
    client: &CoordinatorServiceClient<T>,
    worker_fp: &str,
) -> anyhow::Result<Vec<OpenSession>>
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
    let sessions = response.view().sessions.clone();
    tracing::info!(
        %worker_fp,
        open_sessions = sessions.len(),
        "the coordinator's open-session rows are in hand, so the keeper survivor \
         decision may proceed on a read fact and an adoption may name the session it adopts"
    );
    Ok(sessions)
}

/// The count of the coordinator's open-session rows, for the decision that only
/// needs the number.
///
/// A THIN WRAPPER over [`read_open_sessions`], and deliberately: the keeper
/// decision asks "is anything open" and the adoption asks "which", so a second
/// reader would be a second answer to the same question arriving at two
/// different times.
pub async fn read_open_session_count<T>(
    client: &CoordinatorServiceClient<T>,
    worker_fp: &str,
) -> anyhow::Result<OpenSessionCount>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    Ok(Some(read_open_sessions(client, worker_fp).await?.len()))
}

/// What the boot does with a coordinator that did not answer: `None`.
///
/// A NAMED FUNCTION, and that is the whole point of it. This decision used to
/// be an inline `unwrap_or_else` in the composition root, which is why it
/// shipped untested — a closure in the middle of `serve_until` is not a thing
/// any test can reach, and the failure it guards is invisible in the safe
/// direction: collapse it to `unwrap_or(0)` and `decide` reads "no sessions are
/// open" from a coordinator that said nothing, and a keeper survivor holding
/// somebody's terminals is replaced.
///
/// `None` is not a weaker `Some(0)`. Zero is a CLAIM — this coordinator has no
/// open sessions — and it is the only claim that authorises a replacement.
/// `None` is the absence of a claim, and `keeper_boot::decide` treats it as
/// do-not-replace.
pub fn open_sessions_or_unknown(read: anyhow::Result<OpenSessionCount>) -> OpenSessionCount {
    match read {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(
                %error,
                "boot: the coordinator did not report its open-session set; the keeper \
                 survivor will not be replaced on an unread fact"
            );
            None
        }
    }
}

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

use std::sync::Arc;

use anyhow::Context as _;
use connectrpc::client::ClientTransport;
// `http_body` by way of `connectrpc`, which is the path the sibling
// `bootstrap_redeem` module already uses for the same trait bound: two paths
// to one trait is a second version of it in the lockfile.
use connectrpc::http_body;
use roost_proto::{CoordinatorServiceClient, SessionsListRequest};

use super::boot::WorkerBoot;
use super::keeper_boot::{self, KeeperBootDecision, KeeperBootOutcome};
use super::keeper_handle::KeeperHandle;
use super::keeper_prepare::KeeperProcess;
use crate::keeper_pool::KeeperPool;
use crate::runtime::credential::CredentialSource;
use crate::uplink::OwnerFuture;

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
    credential: &dyn CredentialSource,
) -> anyhow::Result<OpenSessionSet>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    // The worker credential is explicit because boot builds this client
    // separately from the coordinator link.
    let options =
        crate::runtime::bootstrap_redeem::boot_call::authenticated_call_options(credential)
            .context("no worker credential could be presented for the open-session read")?;
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
    // THE OWNED MESSAGE, not `.view()`. The generated client's body is a
    // zero-copy `OwnedView` over the response buffer, and its `sessions` field
    // is a `RepeatedView` of borrowing row views — which borrow from a buffer
    // that dies with the response. This set is held past the call (the keeper
    // decision and the adoption are two different consumers of ONE read), so it
    // has to be `Vec<Session>`. `into_owned` is the conversion the generated
    // code offers, and it is infallible: the bytes already validated as they
    // were decoded.
    let response = client
        .sessions_list_with_options(request, options)
        .await
        .context("the coordinator did not report its open-session set")?
        .into_owned();
    let sessions = response.sessions;
    tracing::info!(
        %worker_fp,
        open_sessions = sessions.len(),
        "the coordinator's open-session rows are in hand, so the keeper survivor \
         decision may proceed on a read fact and an adoption may name the session it adopts"
    );
    Ok(OpenSessionSet { rows: sessions })
}

/// The coordinator's open rows read by `runtime::session_reconcile`.
#[derive(Debug, Default)]
pub struct OpenSessionSet {
    pub rows: Vec<OpenSession>,
}

/// Where a reconcile pass reads the coordinator's open-session rows from.
pub trait OpenSessionSource: Send + Sync {
    fn read(&self) -> OwnerFuture<anyhow::Result<OpenSessionSet>>;
}

/// The production source: `sessionsList` over the boot's Connect client.
pub struct CoordinatorOpenSessions {
    client: CoordinatorServiceClient<connectrpc::client::HttpClient>,
    worker_fp: String,
    /// Held rather than borrowed because this source outlives the boot call
    /// that built it, and the read it performs is the same authenticated one: a
    /// second `sessionsList` without the header would be refused exactly as the
    /// first was, and nothing in either branch's own gate would have seen it,
    /// because the two call sites only meet in this merge.
    credential: Arc<dyn CredentialSource>,
}

impl CoordinatorOpenSessions {
    pub fn new(
        client: CoordinatorServiceClient<connectrpc::client::HttpClient>,
        worker_fp: &str,
        credential: Arc<dyn CredentialSource>,
    ) -> Self {
        Self {
            client,
            worker_fp: worker_fp.to_owned(),
            credential,
        }
    }
}

impl std::fmt::Debug for CoordinatorOpenSessions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordinatorOpenSessions")
            .field("worker_fp", &self.worker_fp)
            .finish_non_exhaustive()
    }
}

impl OpenSessionSource for CoordinatorOpenSessions {
    fn read(&self) -> OwnerFuture<anyhow::Result<OpenSessionSet>> {
        let client = self.client.clone();
        let worker_fp = self.worker_fp.clone();
        let credential = Arc::clone(&self.credential);
        Box::pin(async move { read_open_sessions(&client, &worker_fp, credential.as_ref()).await })
    }
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
    credential: &dyn CredentialSource,
) -> anyhow::Result<OpenSessionCount>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    Ok(Some(
        read_open_sessions(client, worker_fp, credential)
            .await?
            .rows
            .len(),
    ))
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

/// What this worker owns once the keeper has been admitted, and what the
/// coordinator says is still open beside it.
///
/// THE KEEPER HANDLE IS CARRIED AND NOT JUST THE POOL. The boot sequence drops
/// that handle on its way out rather than closing the connection, and the
/// reason — the keeper treats a disconnect as a reason to keep serving — is a
/// property of WHAT THIS WORKER HOLDS, so the handle belongs in the value that
/// says what this worker holds. The pool owns a clone of its own; this is the
/// one the teardown names.
pub struct Reconciled {
    /// The keeper this worker admitted.
    pub keeper: KeeperHandle,
    /// The pool over that keeper, whose dispatch loop is already running.
    pub pool: Arc<KeeperPool>,
    /// The channels that keeper still holds, as the ADMISSION reported them.
    ///
    /// NOT A SECOND READ. The pool learns the ids as a side effect of listing
    /// them, so a boot that listed again would be a second answer to "what does
    /// the keeper hold" arriving after the first was already spent.
    pub survivors: Vec<u16>,
    /// The keeper process this worker started, for a degraded restart.
    pub process: KeeperProcess,
}

impl std::fmt::Debug for Reconciled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // BY COUNTS, not by delegating: the pool and the keeper handle have no
        // `Debug` worth reading, and a boot log line that wants to know what
        // was admitted wants the channel list's length, not its contents.
        formatter
            .debug_struct("Reconciled")
            .field("survivors", &self.survivors.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for KeeperAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owned(reconciled) => formatter
                .debug_tuple("KeeperAdmission::Owned")
                .field(reconciled)
                .finish(),
            Self::Held { decision } => formatter
                .debug_struct("KeeperAdmission::Held")
                .field("decision", decision)
                .finish(),
        }
    }
}

/// What admitting the keeper established.
///
/// NAMED RATHER THAN AN `Option`, because the two answers lead to opposite
/// responses and an `Option` would hide which is which: `Owned` is a boot that
/// can continue, and `Held` is a boot that must refuse to come up at all. The
/// decision travels with it so the caller can name the reason rather than log
/// a word for it.
pub enum KeeperAdmission {
    /// A keeper this worker owns, and what it still holds.
    Owned(Reconciled),
    /// The endpoint is held by a process this worker could not admit. Nothing
    /// was touched.
    Held {
        /// Why the admission declined, for the refusal the caller raises.
        decision: KeeperBootDecision,
    },
}

/// Read the coordinator's open-session set, then admit the keeper against it,
/// in that order.
///
/// THE ORDER IS THE FIX, AND IT IS WHY BOTH HALVES ARE ONE FUNCTION. `decide`
/// takes the open-session count, and a count nobody read is the one value that
/// authorises nothing — so the read and the admission cannot be two steps a
/// caller may sequence wrongly, because there is no sequence to get wrong.
/// `ensure_keeper` used to be called here with `None` for that set, and under
/// the old boot order the `None` was not a gap a later step filled: it was
/// permanent, and a machine whose keeper genuinely needed replacing never
/// replaced it, for ever, with no test noticing because the refusal runs in
/// the safe direction.
///
/// The log directory is not a parameter because [`WorkerBoot`] carries it, and
/// the value the admission receives is the one the boot sequence was passing by
/// hand.
pub async fn admit_keeper<T>(
    boot: &WorkerBoot,
    client: &CoordinatorServiceClient<T>,
    credential: &dyn CredentialSource,
) -> anyhow::Result<KeeperAdmission>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    let open_read = read_open_sessions(client, boot.fingerprint.as_str(), credential).await;
    // `open_sessions_or_unknown` takes a CLAIM — `Result<Option<usize>, _>` —
    // and the rows are borrowed here and moved a few lines down, so the count
    // is derived by reference and the error re-wrapped into an owned one. The
    // `Err` arm still reaches the one log line inside the seam, which is the
    // only thing that function is for.
    let open_sessions = open_sessions_or_unknown(
        open_read
            .as_ref()
            .map(|set| Some(set.rows.len()))
            .map_err(|error| anyhow::anyhow!("{error}")),
    );
    if open_read.as_ref().is_ok_and(|set| set.rows.is_empty()) {
        tracing::info!(
            "boot: the coordinator reports no open session on this worker, so every channel \
             the keeper holds is one it has already closed"
        );
    }
    // The `Held` arm RETURNS rather than yielding `None`, and that is the one
    // behaviour change the move makes: `None` could only ever have come from
    // here, so the caller spent a match arm and a second `Option` to carry a
    // fact this function can simply report.
    let process = KeeperProcess::default();
    let (keeper, survivors) =
        match keeper_boot::ensure_keeper(boot, open_sessions, &boot.log_dir, &process).await {
            Ok(KeeperBootOutcome::Adopted { channels, keeper }) => {
                tracing::info!(
                    ?channels,
                    "boot: adopted the keeper that already holds this machine's terminals"
                );
                (keeper, channels)
            }
            Ok(KeeperBootOutcome::StartedFresh { keeper }) => {
                tracing::info!("boot: started a fresh keeper");
                (keeper, Vec::new())
            }
            Ok(KeeperBootOutcome::Held { decision }) => {
                tracing::warn!(
                    ?decision,
                    "boot: the keeper endpoint is held and nothing was touched"
                );
                return Ok(KeeperAdmission::Held { decision });
            }
            Err(error) => {
                tracing::error!(%error, "boot refused: the keeper endpoint could not be admitted");
                return Err(error);
            }
        };
    // The pool is built from the admitted keeper HERE, and its dispatch loop
    // starts inside `KeeperPool::new` — which is before any history is read.
    // The keeper streams `PtyOut` from the moment a worker connects, and an
    // unbound frame is dropped at `keeper_pool/dispatch.rs`, so a pool that
    // started after the first history read would lose the bytes a survivor
    // produced during the read. Nothing reports that loss: the frames were
    // never expected by anyone.
    Ok(KeeperAdmission::Owned(Reconciled {
        pool: KeeperPool::new(keeper.clone()),
        keeper,
        survivors,
        process,
    }))
}

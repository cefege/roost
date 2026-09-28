//! The one-shot a brand-new machine spends to become a worker: redeem the
//! bootstrap token the service definition carries, then register under the
//! label the fleet shows. Called once per activation by the composition root,
//! before the coordinator link dials, and by nothing else.
//!
//! The token is read HERE and never travels through a struct that outlives the
//! redemption: a one-shot secret held longer than the grant it was is a secret
//! with a longer life than its own value. The label's own resolution, and the
//! reason its ordering is that ordering, is the `label` submodule beside this.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::prelude::{BASE64_STANDARD, Engine as _};
use connectrpc::client::{CallOptions, ClientTransport};
use connectrpc::{ConnectError, ErrorCode, http_body};
use roost_host::{DEV_BUILD_STAMP, EnvSource, HostPlatform, build_identity};
use roost_proto::{AuthRedeemWorkerRequest, CoordinatorServiceClient};
use roost_protocol::wire::WorkerFp;

use self::label::{named, resolve_worker_label};
use self::register::register;
use crate::host::install::{BOOTSTRAP_TOKEN_ENV, scrub_service_definition_env};
use crate::host::jwt::read_existing_worker_key;
use crate::runtime::credential::{CredentialError, CredentialSource};

pub(crate) mod activation;
mod label;
mod register;

pub use activation::enroll_this_activation;

pub use label::ENV_WORKER_LABEL;

/// The address the rest of the fleet reaches this machine at, when an operator
/// named one.
///
/// Absent is a real answer rather than a gap: the coordinator keeps whatever an
/// earlier registration stored, and the heartbeat re-resolves a tailnet name
/// per beat.
pub const ENV_REACHABLE_ADDR: &str = "ROOST_REACHABLE_ADDR";

/// The header the coordinator reads a worker credential out of.
///
/// Spelled here because it cannot be imported: `roost_coord::rpc::auth_gate`
/// keeps the same name private, and a header that does not arrive is an
/// `Unauthenticated` with nothing in it to say the spelling was wrong.
const AUTHORIZATION: &str = "authorization";

/// How long a boot-time coordinator call may take.
///
/// This runs before the link and the heartbeat exist, so a coordinator that
/// accepts the connection and then dies — the operator's front door answers,
/// the dead process behind it never does — would hang the boot forever with no
/// retry loop in range. v2 observed a worker stuck 48 minutes that way.
const BOOT_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Why this machine could not be enrolled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnrollmentError {
    #[error("the worker key at {path} could not be read: {reason}")]
    KeyUnreadable { path: PathBuf, reason: String },
    #[error(
        "no worker label: {ENV_WORKER_LABEL} is unset, this host has no name to give, \
         and HOSTNAME is unset"
    )]
    NoLabel,
    #[error("the coordinator refused the bootstrap redemption: {reason}")]
    RedemptionRefused { reason: String },
    #[error(
        "bootstrap fingerprint mismatch: this machine's key is {expected}, \
         the coordinator redeemed {received}"
    )]
    FingerprintMismatch {
        expected: WorkerFp,
        received: String,
    },
    #[error("the registration could not be authenticated: {reason}")]
    Credential { reason: String },
    #[error("the coordinator refused the registration: {reason}")]
    RegistrationRefused { reason: String },
}

/// What became of the one-shot token. Three answers, not two: the arm that
/// says "it failed" is the one the boot policy actually turns on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redemption {
    /// The environment carried no token, so this machine enrolled on an earlier
    /// activation and is re-registering.
    NotOffered,
    /// The coordinator spent it and bound it to this machine's public key.
    Redeemed,
    /// The coordinator did not answer before the deadline. The call never
    /// reached it, so the token is unspent and the next activation offers it
    /// again.
    Unreachable { reason: String },
}

/// What one enrollment attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrollment {
    /// The fingerprint the coordinator confirmed for this key.
    pub fingerprint: WorkerFp,
    /// The label this machine will appear under in the fleet.
    pub label: String,
    /// What became of the one-shot token.
    pub redemption: Redemption,
    /// Whether the registration landed. `false` is a coordinator that did not
    /// answer, not one that said no: the registration is idempotent and the
    /// link retries it.
    pub registered: bool,
}

/// Enroll this machine: spend the bootstrap token if one was offered, then
/// register.
///
/// The two calls are not peers. `AuthRedeemWorker` is public and is what
/// creates the row; `WorkersRegister` needs a worker credential and refuses a
/// fingerprint the redemption never wrote. So a redemption the coordinator
/// REFUSED is an error here, not a line to log and carry on past: the token is
/// still unspent, the machine still has no authority, and the only thing that
/// would change is how loudly the log says so.
///
/// A call that never reached the coordinator is the other case, and it is
/// tolerated — v2 tolerated EVERY redemption failure under one
/// "may be already used" warning, and this keeps the half of that which was
/// load-bearing. The half that was not is that the token a redeploy re-offers
/// is redeemed idempotently for the key that already holds it, so a spent
/// token is a success and never the failure v2's message described.
///
/// `key_path` is `WorkerBoot::worker_key_path`, the same file the boot sequence
/// derived this worker's fingerprint from, so the key the token is bound to and
/// the key the boot published cannot be two different keys.
pub async fn enroll<T>(
    client: &CoordinatorServiceClient<T>,
    credential: &dyn CredentialSource,
    env: &dyn EnvSource,
    platform: HostPlatform,
    key_path: &Path,
) -> Result<Enrollment, EnrollmentError>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    let label = resolve_worker_label(env, platform, &label::HostLabelSources)?;
    tracing::info!(label = %label, platform = platform.as_str(), "enroll: labelled the machine");

    // A key that cannot be read is a refusal, never an empty key. The whole
    // point of the redemption is to bind this token to THIS machine's public
    // key, so a key read as nothing would spend a one-shot grant on no key at
    // all and leave the machine unauthorized with the token already gone.
    let key =
        read_existing_worker_key(key_path).map_err(|error| EnrollmentError::KeyUnreadable {
            path: key_path.to_path_buf(),
            reason: error.to_string(),
        })?;
    let machine = MachineFacts {
        fingerprint: key.fingerprint().clone(),
        public_key_b64: BASE64_STANDARD.encode(key.public_key()),
        label,
        os: platform.as_str(),
        git_sha: build_sha(env),
        reachable_addr: named(env.get(ENV_REACHABLE_ADDR)),
    };
    tracing::debug!(
        fingerprint = %machine.fingerprint,
        "enroll: read the key this machine redeems with"
    );

    let redemption = redeem(client, env, platform, &machine).await?;
    let registered = register(client, credential, &machine).await?;
    tracing::info!(
        fingerprint = %machine.fingerprint,
        label = %machine.label,
        ?redemption,
        registered,
        "enroll: machine enrollment finished"
    );
    Ok(Enrollment {
        fingerprint: machine.fingerprint,
        label: machine.label,
        redemption,
        registered,
    })
}

/// The facts read once that both calls need.
#[derive(Debug)]
struct MachineFacts {
    fingerprint: WorkerFp,
    public_key_b64: String,
    label: String,
    os: &'static str,
    git_sha: Option<String>,
    reachable_addr: Option<String>,
}

/// Spend the bootstrap token, if this activation was offered one.
async fn redeem<T>(
    client: &CoordinatorServiceClient<T>,
    env: &dyn EnvSource,
    platform: HostPlatform,
    machine: &MachineFacts,
) -> Result<Redemption, EnrollmentError>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    let Some(token) = named(env.get(BOOTSTRAP_TOKEN_ENV)) else {
        tracing::info!("enroll: no bootstrap token offered; this machine is already enrolled");
        return Ok(Redemption::NotOffered);
    };
    let request = AuthRedeemWorkerRequest {
        token,
        ssh_pubkey_b64: machine.public_key_b64.clone(),
        label: machine.label.clone(),
        os: machine.os.to_owned(),
        git_sha: machine.git_sha.clone(),
        ..Default::default()
    };
    tracing::info!(
        fingerprint = %machine.fingerprint,
        "enroll: redeeming the bootstrap token"
    );
    let response = match client
        .auth_redeem_worker_with_options(request, boot_call_options())
        .await
    {
        Ok(response) => response,
        Err(error) if coordinator_is_silent(&error) => {
            tracing::warn!(
                fingerprint = %machine.fingerprint,
                reason = %error,
                "enroll: the coordinator did not answer the redemption; the token is unspent"
            );
            return Ok(Redemption::Unreachable {
                reason: error.to_string(),
            });
        }
        Err(error) => {
            tracing::error!(
                fingerprint = %machine.fingerprint,
                reason = %error,
                "enroll: the coordinator refused the bootstrap token: it is spent by another \
                 key, expired, or not a worker token"
            );
            return Err(EnrollmentError::RedemptionRefused {
                reason: error.to_string(),
            });
        }
    };
    // The response names the key the token was bound to. A name that is not
    // this machine's means the coordinator answered about somebody else, and
    // the only honest continuation is to stop: this worker is now talking as a
    // machine it is not, and every later call would carry that identity.
    let received = response.view().fingerprint;
    if received != machine.fingerprint.as_str() {
        tracing::error!(
            expected = %machine.fingerprint,
            received,
            "enroll: the coordinator redeemed a different key than this machine's"
        );
        return Err(EnrollmentError::FingerprintMismatch {
            expected: machine.fingerprint.clone(),
            received: received.to_owned(),
        });
    }
    tracing::info!(
        fingerprint = %machine.fingerprint,
        label = %machine.label,
        "enroll: bootstrap token redeemed"
    );
    retire_bootstrap_token(env, platform).await;
    Ok(Redemption::Redeemed)
}

/// Erase the spent token from the installed service definition.
///
/// A failure to erase is logged and not returned: the machine IS authorized,
/// and refusing to boot over a text file that could not be edited is a larger
/// incident than the one being prevented. A re-offered spent token costs one
/// idempotent round trip, because the coordinator re-redeems a token for the
/// key that already holds it.
async fn retire_bootstrap_token(env: &dyn EnvSource, platform: HostPlatform) {
    match scrub_service_definition_env(env, platform, BOOTSTRAP_TOKEN_ENV).await {
        Ok(true) => tracing::info!("enroll: bootstrap token erased from the service definition"),
        Ok(false) => {
            tracing::info!("enroll: no service definition carried the bootstrap token to erase")
        }
        Err(error) => tracing::warn!(
            reason = %error,
            "enroll: the service definition could not be stripped of the spent token"
        ),
    }
}

/// Whether a failed call is a coordinator that did not answer rather than one
/// that answered no.
///
/// The two are not the same event and must not be handled the same way: the
/// first is an outage the link recovers from, the second is an authorization
/// this machine did not get and must not go on as though it had.
fn coordinator_is_silent(error: &ConnectError) -> bool {
    matches!(
        error.code,
        ErrorCode::Unavailable | ErrorCode::DeadlineExceeded
    )
}

/// The commit this build reports, when it is a commit.
///
/// The resolution is `roost_host::build_identity`'s, not a second reading of
/// `GIT_SHA`, so the `git_sha` a registration records and the one the hello
/// reports are the same value by construction. A source checkout with no stamp
/// sends nothing, which is what v2 did.
fn build_sha(env: &dyn EnvSource) -> Option<String> {
    let sha = build_identity(env).build_sha;
    if sha.is_empty() || sha == DEV_BUILD_STAMP {
        return None;
    }
    Some(sha)
}

/// The per-call options every boot-time coordinator call carries.
fn boot_call_options() -> CallOptions {
    CallOptions::default().with_timeout(BOOT_CALL_TIMEOUT)
}

/// The per-call options a boot-time coordinator call travels under when it
/// must present this machine's worker credential.
///
/// `pub(crate)` because the open-session read is a boot-time call too
/// ([`crate::runtime::reconcile`]) and it was travelling on a bare client:
/// `SessionsList` is `DeviceOrOwnWorkerRecovery` on the coordinator, so the
/// read that decides keeper admission was refused with an `Unauthenticated`
/// and nothing to point at. One function, because "how does this worker
/// authenticate to its coordinator" having a second answer is the defect this
/// exists to remove — the registration in `register.rs` already spelled it
/// correctly, and this is that spelling with a name.
pub(crate) fn authenticated_call_options(
    credential: &dyn CredentialSource,
) -> Result<CallOptions, CredentialError> {
    let token = credential.mint()?;
    // `try_with_header`, not `with_header`: the latter drops a value it cannot
    // spell, and a credential this worker built wrong would then arrive as an
    // `Unauthenticated` with no header on it and nothing to point at.
    boot_call_options()
        .try_with_header(AUTHORIZATION, format!("Bearer {token}"))
        .map_err(|error| CredentialError::Unspellable {
            reason: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use connectrpc::{ConnectError, ErrorCode};
    use roost_host::{COMPILED_ROOST_BUILD_SHA, MapEnv};

    use super::{build_sha, coordinator_is_silent};

    /// A COMPILED BUILD REPORTS ITS OWN STAMP and ignores the environment; a
    /// source checkout reports the one its service provided. Both halves matter
    /// here, and which one applies is a property of the BUILD — so the test
    /// reads it rather than assuming, exactly as `roost_host::build_identity`'s
    /// own test does.
    #[test]
    fn a_compiled_stamp_is_reported_and_a_source_checkout_sends_nothing() {
        let sourced = MapEnv::new().with("GIT_SHA", "0123456789ab");
        match COMPILED_ROOST_BUILD_SHA {
            // A binary replaced in place must describe itself as what it is,
            // so a stale `GIT_SHA` in the service that launched it cannot
            // rename the release.
            Some(stamped) => {
                assert_eq!(build_sha(&sourced), Some(stamped.to_string()));
                // And a COMPILED build has an answer with no environment at
                // all, which is the half this assertion used to get wrong. It
                // was unconditional, so it asserted a source-checkout property
                // on a build that had a stamp of its own — and the stamp is
                // derived from git at compile time, so whether it is `Some` is
                // a property of the TREE, not of the test. The failure read
                // `left: Some("d7675361...") right: None`, which looks like a
                // build-identity defect and is not one.
                assert_eq!(build_sha(&MapEnv::new()), Some(stamped.to_string()));
            }
            // A checkout with no stamp of its own has nothing else to send.
            None => {
                assert_eq!(build_sha(&sourced), Some("0123456789ab".to_string()));
                assert_eq!(build_sha(&MapEnv::new()), None);
            }
        }
    }

    #[test]
    fn only_a_coordinator_that_did_not_answer_counts_as_silent() {
        // An outage the link recovers from is tolerated; an authorization this
        // machine did not get is not, and the two must not collapse into one
        // arm the way v2's single catch did.
        for silent in [ErrorCode::Unavailable, ErrorCode::DeadlineExceeded] {
            let error = ConnectError::new(silent, "no answer");
            assert!(coordinator_is_silent(&error), "{silent:?} was not silent");
        }
        for spoken in [
            ErrorCode::Unauthenticated,
            ErrorCode::InvalidArgument,
            ErrorCode::PermissionDenied,
            ErrorCode::Internal,
            ErrorCode::Unimplemented,
        ] {
            let error = ConnectError::new(spoken, "no");
            assert!(!coordinator_is_silent(&error), "{spoken:?} was silent");
        }
    }
}

//! The one call the boot sequence makes to become a member of the fleet, and
//! the two answers it may give. `runtime::serve_until` calls
//! [`enroll_this_activation`] after the identity is settled and before the
//! coordinator link dials; nothing else calls it.
//!
//! WHY BEFORE THE LINK. A link that opens before the coordinator has this
//! machine's `authorized_keys` row is a link whose first frame arrives at a
//! coordinator that does not know the sender, and the retry that follows is a
//! reconnect rather than a registration. Enrolling first means the coordinator
//! already knows the machine when the hello lands.
//!
//! WHY THE TOKEN IS NOT AN ARGUMENT. [`BOOTSTRAP_TOKEN_ENV`] is read HERE, at
//! the one moment the value is needed, and the value never becomes a field, a
//! local that outlives the call, or a log line. It is a one-shot grant: a copy
//! held across a reconnect loop is a credential with a longer life than the
//! grant it authorises.
//!
//! TLS IS NOT CONFIGURED, AND A CLEARTEXT FALLBACK IS NOT ITS REPAIR. This
//! worker configures no TLS connector anywhere — `link_dial` says so of the
//! `wss` dial in the same terms — so an `https` coordinator is refused by name
//! rather than dialled in the clear. An operator who needs TLS needs it on the
//! link too, and half of it is not half of a fix.

use anyhow::Context as _;
use connectrpc::client::{ClientConfig, HttpClient};
use roost_host::{EnvSource, ProcessEnv, supported_host_platform};
use roost_proto::CoordinatorServiceClient;

use super::{Enrollment, enroll};
use crate::host::install::BOOTSTRAP_TOKEN_ENV;
use crate::runtime::boot::WorkerBoot;
use crate::runtime::credential::WorkerKeyCredential;

/// Enroll this machine, if this activation was offered a token to spend.
///
/// `Ok(None)` is the ordinary answer for every activation after the first: the
/// environment named no token, so there is nothing to redeem and NO coordinator
/// call is made at all. A machine that is already enrolled still gets its
/// registration below — that call is idempotent, and skipping it is what left a
/// redeployed worker holding a key the coordinator had never been told about.
///
/// `Err` is a boot refusal, not a warning: a redemption the coordinator
/// REJECTED leaves the machine unauthorized, and continuing would join a link
/// loop that fails in silence every thirty seconds. A coordinator that did not
/// answer is not this — that arrives as `Ok(Some(..))` with
/// `Redemption::Unreachable`, and the link retries it.
pub async fn enroll_this_activation(boot: &WorkerBoot) -> anyhow::Result<Option<Enrollment>> {
    let env = ProcessEnv::new();
    if !token_offered(&env) {
        tracing::info!(
            fingerprint = %boot.fingerprint,
            "enroll: this activation was offered no bootstrap token; the machine keeps the one it has"
        );
        return Ok(None);
    }
    let client = coordinator_client(&boot.coordinator_base)?;
    let platform = supported_host_platform().context("this host is not one v3 supports")?;
    let credential = WorkerKeyCredential::new(boot.worker_key_path.clone());
    let enrollment = enroll(
        &client,
        &credential,
        &env,
        platform,
        &boot.worker_key_path,
    )
    .await
    .context("this machine could not be enrolled")?;
    Ok(Some(enrollment))
}

/// Whether this activation carries a token at all.
///
/// Presence only, deliberately: the value is not bound, not returned, and not
/// logged. The redemption reads it for itself at the point of use, so this
/// question and that read cannot drift apart — there is one source, and it is
/// the environment.
fn token_offered(env: &dyn EnvSource) -> bool {
    env.get(BOOTSTRAP_TOKEN_ENV).is_some_and(|token| !token.is_empty())
}

/// The Connect client the two boot-time calls travel over.
///
/// `pub(crate)` because boot makes a THIRD call on the same connection — the
/// open-session read in [`crate::runtime::reconcile`] — and a second client
/// constructor would be a second answer to "how does this worker reach its
/// coordinator", including the same refusal to dial `https` in the clear. The
/// TLS refusal above is the reason this is one function rather than two.
pub(crate) fn coordinator_client(
    base: &str,
) -> anyhow::Result<CoordinatorServiceClient<HttpClient>> {
    let uri: axum::http::Uri = base
        .parse()
        .with_context(|| format!("{base} is not a coordinator URL"))?;
    match uri.scheme_str() {
        Some("http") => Ok(CoordinatorServiceClient::new(
            HttpClient::plaintext(),
            ClientConfig::new(uri),
        )),
        Some("https") => anyhow::bail!(
            "this worker configures no TLS connector, so {base} cannot be enrolled against; \
             configure TLS for the coordinator link as well, or point the worker at an \
             http coordinator"
        ),
        other => anyhow::bail!("{base} is not a coordinator URL: the scheme is {other:?}"),
    }
}

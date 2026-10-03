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
//! AN `https` COORDINATOR IS DIALLED OVER TLS, NEVER IN THE CLEAR. Both legs —
//! this Connect client and the `wss` link in `link_dial` — take their TLS from
//! [`crate::coordinator_tls`], so the two trust the same roots. v2's workers
//! dial `https://` front doors, and a worker that could not would join nothing
//! but a coordinator on its own machine.
//! Ports v2 `apps/worker/src/host/install.ts`.

use anyhow::Context as _;
use connectrpc::client::{ClientConfig, HttpClient};
use roost_host::{EnvSource, ProcessEnv, supported_host_platform};
use roost_proto::CoordinatorServiceClient;

use super::{Enrollment, enroll};
use crate::coordinator_tls::coordinator_tls_config;
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
    let enrollment = enroll(&client, &credential, &env, platform, &boot.worker_key_path)
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
    env.get(BOOTSTRAP_TOKEN_ENV)
        .is_some_and(|token| !token.is_empty())
}

/// The Connect client the two boot-time calls travel over.
///
/// `pub(crate)` because boot makes a THIRD call on the same connection — the
/// open-session read in [`crate::runtime::reconcile`] — and a second client
/// constructor would be a second answer to "how does this worker reach its
/// coordinator", including what TLS an `https` coordinator is dialled with.
/// One answer to that is the reason this is one function rather than two.
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
        Some("https") => {
            let tls = coordinator_tls_config()
                .context("the TLS client configuration for the coordinator could not be built")?;
            Ok(CoordinatorServiceClient::new(
                HttpClient::with_tls(tls),
                ClientConfig::new(uri),
            ))
        }
        other => anyhow::bail!("{base} is not a coordinator URL: the scheme is {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::coordinator_client;

    /// v2's workers dial `https://` front doors, and `roost add-machine` only
    /// hands out HTTPS origins, so an `https` base must build a TLS client. It
    /// used to be refused by name, which left every remote join looping on
    /// "is not a coordinator this worker can dial".
    #[tokio::test]
    async fn an_https_coordinator_gets_a_tls_client_rather_than_a_refusal() {
        assert!(coordinator_client("https://coordinator.example").is_ok());
        assert!(coordinator_client("http://127.0.0.1:4113").is_ok());
        assert!(coordinator_client("ftp://coordinator.example").is_err());
    }
}

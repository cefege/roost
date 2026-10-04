//! The second half of enrollment: telling the coordinator what this machine is
//! called and where it can be reached, under a credential the coordinator can
//! verify. Called by [`super::enroll`] after — and only ever after — the
//! redemption, because the coordinator refuses a fingerprint its `authorized_keys`
//! row does not already name.
//!
//! Every fact this call sends was decided upstream: the label by
//! [`super::label`], the key by `runtime::boot`, the commit by
//! `roost_host::build_identity`. What lives here is the credential, the two
//! ways the call can fail, which are not the same event, and the fallback a
//! refused token takes when the key is already registered.
//! Ports v2 `apps/worker/src/transport/coord-client.ts`, `apps/worker/src/host/install.ts`.

use connectrpc::client::ClientTransport;
use connectrpc::http_body;
use roost_host::{EnvSource, HostPlatform};
use roost_proto::buffa::MessageField;
use roost_proto::{CoordinatorServiceClient, WorkersRegisterRequest};
use roost_protocol::proto_adapters::host_identity_to_proto;

use super::boot_call::authenticated_call_options;
use super::{
    Enrollment, EnrollmentError, MachineFacts, Redemption, coordinator_is_silent,
    retire_bootstrap_token,
};
use crate::host::identity::static_host_identity;
use crate::runtime::credential::CredentialSource;

/// Register under the label, idempotently, with a credential the coordinator
/// can verify.
///
/// `Ok(false)` is a coordinator that did not answer, and nothing else: the
/// registration is idempotent and the link retries it, so the only thing a
/// boot needs to know is that the heartbeat will finish the job. A
/// registration the coordinator REFUSED is the opposite — this machine holds
/// a credential the coordinator will not accept, and no retry makes that true
/// — so it stops the boot rather than joining a loop that fails in silence
/// every thirty seconds.
pub(super) async fn register<T>(
    client: &CoordinatorServiceClient<T>,
    credential: &dyn CredentialSource,
    machine: &MachineFacts,
) -> Result<bool, EnrollmentError>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    // ONE mechanism, shared with the open-session read: this call and that one
    // are both boot-time calls to the same coordinator, and a second spelling
    // of the credential header is how the read ended up travelling without one.
    let options = match authenticated_call_options(credential) {
        Ok(options) => options,
        Err(error) => {
            tracing::error!(
                fingerprint = %machine.fingerprint,
                reason = %error,
                "enroll: the registration could not be authenticated"
            );
            return Err(EnrollmentError::Credential {
                reason: error.to_string(),
            });
        }
    };
    // The host identity is always sent, empty fields and all. The coordinator
    // normalises an identity whose every field is absent to the same "nothing
    // was collected" it answers for a missing message, so sending it costs
    // nothing and removes the branch that would have to guess which of the
    // two it is.
    let request = WorkersRegisterRequest {
        label: Some(machine.label.clone()),
        os: Some(machine.os.to_owned()),
        git_sha: machine.git_sha.clone(),
        reachable_addr: machine.reachable_addr.clone(),
        host_identity: MessageField::some(host_identity_to_proto(static_host_identity().as_ref())),
        ..Default::default()
    };
    tracing::info!(
        fingerprint = %machine.fingerprint,
        "enroll: registering with the coordinator"
    );
    match client.workers_register_with_options(request, options).await {
        Ok(_) => {
            tracing::info!(
                fingerprint = %machine.fingerprint,
                "enroll: registered with the coordinator"
            );
            Ok(true)
        }
        Err(error) if coordinator_is_silent(&error) => {
            tracing::warn!(
                fingerprint = %machine.fingerprint,
                reason = %error,
                "enroll: the coordinator did not answer the registration; the link retries it"
            );
            Ok(false)
        }
        Err(error) => {
            tracing::error!(
                fingerprint = %machine.fingerprint,
                reason = %error,
                "enroll: the coordinator refused the registration"
            );
            Err(EnrollmentError::RegistrationRefused {
                reason: error.to_string(),
            })
        }
    }
}

/// A refused token on a machine whose key the coordinator already knows.
///
/// A service definition can outlive the one-shot it carried — a reinstall
/// that re-wrote it, a scrub that failed — and the token it re-offers is spent
/// or expired. The key, not the token, is what this machine's authority rests
/// on once enrolled, so the registration decides: accepted means the token is
/// stale and is erased; anything else means the machine truly is unauthorized,
/// and the ORIGINAL refusal is what the boot reports.
pub(super) async fn enroll_with_existing_registration<T>(
    client: &CoordinatorServiceClient<T>,
    credential: &dyn CredentialSource,
    env: &dyn EnvSource,
    platform: HostPlatform,
    machine: MachineFacts,
    refusal: EnrollmentError,
) -> Result<Enrollment, EnrollmentError>
where
    T: ClientTransport,
    <T::ResponseBody as http_body::Body>::Error: std::fmt::Display,
{
    tracing::warn!(
        fingerprint = %machine.fingerprint,
        "enroll: the bootstrap token was refused; trying the key's existing registration"
    );
    match register(client, credential, &machine).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::error!(
                fingerprint = %machine.fingerprint,
                "enroll: the coordinator did not answer the registration after refusing the token"
            );
            return Err(refusal);
        }
        Err(error) => {
            tracing::error!(
                fingerprint = %machine.fingerprint,
                %error,
                "enroll: the key has no registration to fall back on"
            );
            return Err(refusal);
        }
    }
    retire_bootstrap_token(env, platform).await;
    tracing::info!(
        fingerprint = %machine.fingerprint,
        label = %machine.label,
        "enroll: an enrolled key registered past a stale bootstrap token"
    );
    Ok(Enrollment {
        fingerprint: machine.fingerprint,
        label: machine.label,
        redemption: Redemption::AlreadyEnrolled,
        registered: true,
    })
}

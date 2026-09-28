//! The second half of enrollment: telling the coordinator what this machine is
//! called and where it can be reached, under a credential the coordinator can
//! verify. Called by [`super::enroll`] after — and only ever after — the
//! redemption, because the coordinator refuses a fingerprint its `authorized_keys`
//! row does not already name.
//!
//! Every fact this call sends was decided upstream: the label by
//! [`super::label`], the key by `runtime::boot`, the commit by
//! `roost_host::build_identity`. What lives here is the credential and the two
//! ways the call can fail, which are not the same event.
//! Ports v2 `apps/worker/src/transport/coord-client.ts`, `apps/worker/src/host/install.ts`.

use connectrpc::client::ClientTransport;
use connectrpc::http_body;
use roost_proto::buffa::MessageField;
use roost_proto::{CoordinatorServiceClient, WorkersRegisterRequest};
use roost_protocol::proto_adapters::host_identity_to_proto;

use super::{
    AUTHORIZATION, EnrollmentError, MachineFacts, boot_call_options, coordinator_is_silent,
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
    let token = match credential.mint() {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(
                fingerprint = %machine.fingerprint,
                reason = %error,
                "enroll: no worker credential could be minted for the registration"
            );
            return Err(EnrollmentError::Credential {
                reason: error.to_string(),
            });
        }
    };
    // `try_with_header`, not `with_header`: the latter drops a value it cannot
    // spell, and a credential this worker built wrong would then arrive as an
    // `Unauthenticated` with no header on it and nothing to point at.
    let options =
        match boot_call_options().try_with_header(AUTHORIZATION, format!("Bearer {token}")) {
            Ok(options) => options,
            Err(error) => {
                tracing::error!(
                    fingerprint = %machine.fingerprint,
                    reason = %error,
                    "enroll: the worker credential is not a header value"
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

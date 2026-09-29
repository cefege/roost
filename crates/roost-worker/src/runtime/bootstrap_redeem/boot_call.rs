//! How a boot-time coordinator call presents this machine, and how long it is
//! given to answer. Owned by `bootstrap_redeem`, which spends the one-shot
//! token; consumed by the open-session read in `runtime::reconcile`, which is a
//! boot-time call too.
//!
//! One place, because "how does this worker authenticate to its coordinator"
//! having a second answer is the defect these exist to remove: the registration
//! in `register.rs` already spelled it correctly, and this is that spelling with
//! a name.

use std::time::Duration;

use connectrpc::client::CallOptions;

use crate::runtime::credential::{CredentialError, CredentialSource};

/// The header the coordinator reads a worker credential out of.
///
/// Spelled here because it cannot be imported: `roost_coord::rpc::auth_gate`
/// keeps the same name private, and a header that does not arrive is an
/// `Unauthenticated` with nothing in it to say the spelling was wrong.
pub(crate) const AUTHORIZATION: &str = "authorization";

/// How long a boot-time coordinator call may take.
///
/// This runs before the link and the heartbeat exist, so a coordinator that
/// accepts the connection and then dies — the operator's front door answers,
/// the dead process behind it never does — would hang the boot forever with no
/// retry loop in range. v2 observed a worker stuck 48 minutes that way.
const BOOT_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The per-call options every boot-time coordinator call carries.
pub(super) fn boot_call_options() -> CallOptions {
    CallOptions::default().with_timeout(BOOT_CALL_TIMEOUT)
}

/// The per-call options a boot-time coordinator call travels under when it
/// must present this machine's worker credential.
///
/// `pub(crate)` because the open-session read is a boot-time call too
/// ([`crate::runtime::reconcile`]) and it was travelling on a bare client:
/// `SessionsList` is `DeviceOrOwnWorkerRecovery` on the coordinator, so the
/// read that decides keeper admission was refused with an `Unauthenticated`
/// and nothing to point at.
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

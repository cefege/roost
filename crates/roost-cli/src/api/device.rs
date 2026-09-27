//! `roost api device-revoke-local`: revoke one enrolled device on the
//! coordinator on this machine, without a credential. Called by `api::mod`;
//! depends on the generated `DevicesRevoke` method and on `api::credentials`.
//!
//! WHY THIS VERB IS LOOPBACK-ONLY AND UNAUTHENTICATED, AND WHY THAT IS NOT A
//! HOLE. A browser that has lost its key cannot re-pair itself, and the only
//! person who can fix that is someone sitting at the coordinator's own machine
//! with a shell. So this one call is allowed to arrive without a bearer — and
//! it is allowed only from `http://127.0.0.1:<port>`, because the reach it
//! needs is a reach nothing else may have. Every other verb goes through
//! `api::client` and presents the credential like everything else.
//!
//! WHY `--yes` IS REQUIRED. Revoking a device is not readable and not
//! reversible from the device's side: the next call it makes is refused, and
//! the operator has to re-pair by hand. A flag the operator types is the
//! smallest thing that distinguishes that from a mistyped fingerprint.

use std::process::ExitCode;

use roost_host::{EnvSource, HostPlatform};
use roost_proto::DevicesRevokeRequest;

use crate::api::client::CoordinatorApi;
use crate::api::credentials;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// The only host a device may be revoked from.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// Revoke one device on this machine's coordinator.
pub async fn revoke_local(
    env: &dyn EnvSource,
    platform: HostPlatform,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let fingerprint = args.positional(0, "fingerprint")?;
    if !args.has("--yes") {
        return Err(CommandFailure::usage(format!(
            "roost api device-revoke-local: revoking {fingerprint} takes effect immediately and \
             the device cannot re-pair itself. Pass --yes."
        )));
    }
    let origin = loopback_origin(env, platform)?;
    let api = CoordinatorApi::at(&origin, None)?;
    let ok = api
        .answer(api.stub().devices_revoke(DevicesRevokeRequest {
            fingerprint: fingerprint.to_string(),
            ..Default::default()
        }))
        .await?
        .ok;
    out.answer(&ok.to_string());
    Ok(ExitCode::SUCCESS)
}

/// The coordinator's own loopback listener, and the refusal for anything else.
fn loopback_origin(env: &dyn EnvSource, platform: HostPlatform) -> Result<String, CommandFailure> {
    let declared = credentials::origin(env, platform)?;
    let rest = declared
        .strip_prefix("http://")
        .ok_or_else(|| not_loopback(&declared))?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| not_loopback(&declared))?;
    if host != LOOPBACK_HOST || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(not_loopback(&declared));
    }
    Ok(declared)
}

fn not_loopback(origin: &str) -> CommandFailure {
    CommandFailure::usage(format!(
        "roost api device-revoke-local: it reaches the coordinator without a credential, so it \
         only runs against this machine's own listener — {origin} is not http://{LOOPBACK_HOST}:\
         <port>"
    ))
}

#[cfg(test)]
mod tests {
    use super::{LOOPBACK_HOST, not_loopback};
    use crate::command_error::CommandFailure;

    #[test]
    fn a_refusal_names_the_host_it_would_have_accepted() {
        let failure = not_loopback("https://coord.example");
        let CommandFailure { code, message } = failure;
        assert_eq!(code, crate::command_error::REJECTED_INVOCATION);
        assert!(message.contains(LOOPBACK_HOST), "{message}");
    }
}

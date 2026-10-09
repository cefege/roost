//! `roost api devices` and `device-revoke` (authenticated, any coordinator),
//! and `device-revoke-local`: revoke one enrolled device on the coordinator on
//! this machine, without a credential. Called by `api::mod`; depends on the
//! generated `Devices*` methods and on `api::credentials`.
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
use roost_proto::{DevicesListRequest, DevicesRevokeRequest};

use crate::api::client::CoordinatorApi;
use crate::api::credentials;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// Every enrolled device, one row each; `*` marks the one asking.
pub async fn list(
    api: &CoordinatorApi,
    _args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().devices_list(DevicesListRequest::default()))
        .await?;
    out.answer("fingerprint\tlabel\tadded_at_ms\tpaired_from");
    for device in &response.devices {
        out.answer(&format!(
            "{}{}\t{}\t{}\t{}",
            if device.is_self { "*" } else { "" },
            device.fingerprint,
            device.label,
            device.added_at_ms,
            device.paired_from_ip
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// Revoke one device, named by fingerprint, unique fingerprint prefix or
/// exact label. Refused for an ambiguous name, and without `--yes`.
pub async fn revoke(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let named = args.positional(0, "fingerprint|prefix|label")?;
    let response = api
        .answer(api.stub().devices_list(DevicesListRequest::default()))
        .await?;
    let rows: Vec<(&str, &str)> = response
        .devices
        .iter()
        .map(|device| (device.fingerprint.as_str(), device.label.as_str()))
        .collect();
    let fingerprint = resolve_device(&rows, named)?;
    if !args.has("--yes") {
        return Err(CommandFailure::usage(format!(
            "roost api device-revoke: revoking {fingerprint} takes effect immediately and the \
             device cannot re-pair itself. Pass --yes."
        )));
    }
    let ok = api
        .answer(api.stub().devices_revoke(DevicesRevokeRequest {
            fingerprint: fingerprint.clone(),
            ..Default::default()
        }))
        .await?
        .ok;
    out.answer(&format!(
        "{fingerprint}\t{}",
        if ok { "revoked" } else { "not revoked" }
    ));
    Ok(ExitCode::SUCCESS)
}

/// The one device `named` picks: an exact fingerprint, a unique prefix, or a
/// unique exact label.
pub fn resolve_device(rows: &[(&str, &str)], named: &str) -> Result<String, CommandFailure> {
    if let Some((fingerprint, _)) = rows.iter().find(|(fingerprint, _)| *fingerprint == named) {
        return Ok((*fingerprint).to_owned());
    }
    let matches: Vec<&str> = rows
        .iter()
        .filter(|(fingerprint, label)| fingerprint.starts_with(named) || *label == named)
        .map(|(fingerprint, _)| *fingerprint)
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(CommandFailure::usage(format!(
            "roost api device-revoke: no device is named {named}"
        ))),
        many => Err(CommandFailure::usage(format!(
            "roost api device-revoke: {named} names {} devices: {}",
            many.len(),
            many.join(", ")
        ))),
    }
}

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
    use super::{LOOPBACK_HOST, not_loopback, resolve_device};

    #[test]
    fn a_device_is_named_by_fingerprint_prefix_or_label_and_never_ambiguously() {
        let rows = [
            ("aa11", "laptop"),
            ("aa22", "agent-live-test"),
            ("bb33", "phone"),
        ];
        assert_eq!(resolve_device(&rows, "bb").unwrap(), "bb33");
        assert_eq!(resolve_device(&rows, "agent-live-test").unwrap(), "aa22");
        assert_eq!(resolve_device(&rows, "aa22").unwrap(), "aa22");
        assert!(
            resolve_device(&rows, "aa").is_err(),
            "a shared prefix is ambiguous"
        );
        assert!(resolve_device(&rows, "zz").is_err());
    }
    use crate::command_error::CommandFailure;

    #[test]
    fn a_refusal_names_the_host_it_would_have_accepted() {
        let failure = not_loopback("https://coord.example");
        let CommandFailure { code, message } = failure;
        assert_eq!(code, crate::command_error::REJECTED_INVOCATION);
        assert!(message.contains(LOOPBACK_HOST), "{message}");
    }
}

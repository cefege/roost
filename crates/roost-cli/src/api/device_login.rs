//! `roost api login` and `roost api logout`: enrol this machine's `roost api`
//! as a device by spending a one-shot pairing grant (the URL `roost
//! add-browser` prints), and revoke it again. Called by `api::mod`; the key
//! and its storage are `api::cli_device`'s.

use std::process::ExitCode;

use roost_host::EnvSource;
use roost_proto::{AuthRedeemBrowserRequest, DevicesListRequest, DevicesRevokeRequest};

use crate::api::cli_device::{self, CliDevice};
use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// The label a login records when the operator names none.
const DEFAULT_LABEL: &str = "roost api";

/// The coordinator origin and the grant token a pairing argument names.
///
/// Accepts the URL `roost add-browser` prints (`https://host/#pair=TOKEN`) or
/// a bare token with `--url <origin>`.
pub fn pairing_target(
    argument: &str,
    url: Option<&str>,
) -> Result<(String, String), CommandFailure> {
    if let Some((base, token)) = argument.split_once("#pair=") {
        let origin = base.trim_end_matches('/');
        if !(origin.starts_with("http://") || origin.starts_with("https://")) || token.is_empty() {
            return Err(CommandFailure::usage(format!(
                "roost api login: {argument} is not a pairing URL"
            )));
        }
        return Ok((origin.to_owned(), token.to_owned()));
    }
    let origin = url.ok_or_else(|| {
        CommandFailure::usage(
            "roost api login: pass the pairing URL `roost add-browser` printed, or a token with \
             --url <coordinator origin>",
        )
    })?;
    Ok((origin.trim_end_matches('/').to_owned(), argument.to_owned()))
}

/// Spend a pairing grant for a fresh device key and store it.
pub async fn login(
    env: &dyn EnvSource,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let (origin, token) = pairing_target(
        args.positional(0, "pairing-url")?,
        args.optional_value("--url"),
    )?;
    let label = args.optional_value("--label").unwrap_or(DEFAULT_LABEL);
    let device = CliDevice::generate(&origin)?;
    let anonymous = CoordinatorApi::at(&origin, None)?;
    anonymous
        .answer(
            anonymous
                .stub()
                .auth_redeem_browser(AuthRedeemBrowserRequest {
                    token,
                    ssh_pubkey_b64: device.public_key_b64()?,
                    label: label.to_owned(),
                    ..Default::default()
                }),
        )
        .await?;
    // Prove the enrolment before keeping it: a key the coordinator does not
    // recognise would make every later verb fail far from its cause.
    let enrolled = CoordinatorApi::at(&origin, Some(&device.bearer(cli_device::now_ms())?))?;
    let devices = enrolled
        .answer(enrolled.stub().devices_list(DevicesListRequest::default()))
        .await?;
    if !devices
        .devices
        .iter()
        .any(|row| row.fingerprint == device.fingerprint)
    {
        return Err(CommandFailure::generic(
            "the grant was spent but the coordinator does not list this device",
        ));
    }
    let path = cli_device::save(env, &device)?;
    out.progress(&format!("stored the device key in {}", path.display()));
    out.answer(&format!(
        "logged in to {origin} as device {}",
        device.fingerprint
    ));
    Ok(ExitCode::SUCCESS)
}

/// Revoke this machine's enrolled device and forget its key.
pub async fn logout(
    env: &dyn EnvSource,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let Some(device) = cli_device::load(env) else {
        out.answer("not logged in");
        return Ok(ExitCode::SUCCESS);
    };
    let api = CoordinatorApi::at(&device.origin, Some(&device.bearer(cli_device::now_ms())?))?;
    api.answer(api.stub().devices_revoke(DevicesRevokeRequest {
        fingerprint: device.fingerprint.clone(),
        ..Default::default()
    }))
    .await?;
    cli_device::remove(env)?;
    out.answer(&format!(
        "revoked device {} and forgot its key",
        device.fingerprint
    ));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::pairing_target;

    #[test]
    fn a_pairing_url_names_its_origin_and_token() {
        assert_eq!(
            pairing_target("https://mike.example/#pair=roost_bt_abc", None).unwrap(),
            ("https://mike.example".to_owned(), "roost_bt_abc".to_owned())
        );
        assert_eq!(
            pairing_target("roost_bt_abc", Some("http://127.0.0.1:4113/")).unwrap(),
            (
                "http://127.0.0.1:4113".to_owned(),
                "roost_bt_abc".to_owned()
            )
        );
        assert!(pairing_target("roost_bt_abc", None).is_err());
        assert!(pairing_target("ftp://x/#pair=t", None).is_err());
    }
}

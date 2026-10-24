//! Authenticated one-shot enrollment grants for headless operators.
//!
//! Called by `roost api` after its normal device credential is resolved. It
//! depends on the generated coordinator client and prints the bearer only as
//! the command's requested stdout answer.

use std::process::ExitCode;

use roost_proto::AuthMintBootstrapRequest;

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// Mint one browser or worker enrollment grant through the coordinator API.
pub async fn mint(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let kind = args.positional(0, "kind")?;
    if !matches!(kind, "browser" | "worker") {
        return Err(CommandFailure::usage(
            "roost api bootstrap-mint: <kind> must be browser or worker",
        ));
    }
    let label = args.optional_value("--label").unwrap_or(kind);
    let response = api
        .answer(api.stub().auth_mint_bootstrap(AuthMintBootstrapRequest {
            kind: kind.to_owned(),
            label: label.to_owned(),
            ..Default::default()
        }))
        .await?;
    if response.token.is_empty() {
        return Err(CommandFailure::generic(
            "the coordinator minted an empty bootstrap grant",
        ));
    }
    out.answer(&response.token);
    out.progress(&format!(
        "Minted one-shot {kind} grant; it expires at {}.",
        response.expires_at_ms
    ));
    Ok(ExitCode::SUCCESS)
}

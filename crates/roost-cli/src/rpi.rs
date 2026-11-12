//! Machine-local Rpi provider login, logout and available chat identifiers.
//! The command dispatcher calls this module; provider files and catalog loading
//! belong to the worker's shared ProviderLoader, never a coordinator account store.

pub mod interaction;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueEnum};
use roost_worker::rpi::provider::ProviderLoader;
use rpi_ai::auth::{AuthInteraction, AuthPrompt, AuthType, LoginOptions, ModelsError, ModelsErrorCode, SelectOption};
use rpi_ai::models::Models;
use tokio_util::sync::CancellationToken;

use crate::command_error::CommandFailure;
use interaction::TerminalInteraction;

#[derive(Debug, Args)]
pub struct RpiArgs {
    #[command(subcommand)]
    pub command: RpiCommand,
}

#[derive(Debug, Subcommand)]
pub enum RpiCommand {
    /// Configure a provider on this worker machine; credentials are entered privately.
    Login {
        provider: String,
        #[arg(long, value_enum)]
        auth: Option<LoginAuth>,
    },
    /// Remove a provider's stored credentials from this worker machine.
    Logout { provider: String },
    /// List authenticated available chat provider/model identifiers on this machine.
    Models,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LoginAuth {
    ApiKey,
    Oauth,
}

pub async fn run(args: &RpiArgs) -> Result<ExitCode, CommandFailure> {
    let worker_data = roost_host::worker_data_dir(
        &roost_host::ProcessEnv::new(),
        roost_host::supported_host_platform()?,
    )?;
    let loader = ProviderLoader::new(worker_data);
    match &args.command {
        RpiCommand::Models => {
            let models = loader.available_models().await.map_err(provider_failure)?;
            if models.is_empty() {
                println!("No available Rpi chat models. On this machine, run: roost rpi login <provider>");
            } else {
                for model in models {
                    println!("{}/{}", model.provider, model.model_id);
                }
            }
        }
        RpiCommand::Login { provider, auth } => {
            let snapshot = loader.load().await.map_err(provider_failure)?;
            login(&snapshot.models, provider, *auth).await?;
            println!("Rpi login saved on this machine.");
        }
        RpiCommand::Logout { provider } => {
            let snapshot = loader.load().await.map_err(provider_failure)?;
            snapshot.models.logout(provider).await.map_err(|error| auth_failure("logout", &error))?;
            println!("Rpi stored credentials removed from this machine. Environment/config credentials are unchanged.");
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn login(models: &Models, provider_id: &str, requested: Option<LoginAuth>) -> Result<(), CommandFailure> {
    let provider = models.get_provider(provider_id).ok_or_else(|| CommandFailure::usage("Unknown Rpi provider; use a built-in or models.json provider identifier."))?;
    let methods = provider.auth();
    let api_key = methods.api_key.as_ref().is_some_and(|method| method.supports_login());
    let oauth = methods.oauth.is_some();
    let requested = match requested {
        Some(LoginAuth::ApiKey) if api_key => Some(AuthType::ApiKey),
        Some(LoginAuth::Oauth) if oauth => Some(AuthType::Oauth),
        Some(_) => return Err(CommandFailure::usage("This Rpi provider does not support the requested login method.")),
        None => match (api_key, oauth) {
            (true, false) => Some(AuthType::ApiKey),
            (false, true) => Some(AuthType::Oauth),
            (true, true) => None,
            (false, false) => return Err(CommandFailure::usage("This Rpi provider has no interactive login method; configure its worker-local models.json or supported environment credentials.")),
        },
    };
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(CommandFailure::usage("Rpi login requires an interactive terminal; credentials cannot be supplied as arguments or piped input."));
    }
    let cancellation = CancellationToken::new();
    let interaction = TerminalInteraction::new(cancellation.clone());
    let options = LoginOptions { agent_name: Some("Roost".to_owned()), ..Default::default() };
    let login = async {
        let method = match requested {
            Some(method) => method,
            None => {
                let selected = interaction.prompt(AuthPrompt::Select {
                    message: "Login method".to_owned(),
                    options: vec![
                        SelectOption { id: "api-key".to_owned(), label: "API key".to_owned(), description: None },
                        SelectOption { id: "oauth".to_owned(), label: "OAuth".to_owned(), description: None },
                    ],
                    signal: None,
                }).await?;
                match selected.as_str() {
                    "api-key" => AuthType::ApiKey,
                    "oauth" => AuthType::Oauth,
                    _ => return Err(ModelsError::aborted()),
                }
            }
        };
        models.login(provider_id, method, &interaction, Some(&options)).await.map(|_| ())
    };
    tokio::pin!(login);
    let result = tokio::select! {
        result = &mut login => result,
        signal = tokio::signal::ctrl_c() => {
            cancellation.cancel();
            if signal.is_err() {
                return Err(CommandFailure::generic("Rpi login could not install terminal cancellation."));
            }
            // The library must finish a credential mutation that has already begun.
            login.await
        }
    };
    result.map_err(|error| auth_failure("login", &error))
}

fn provider_failure(error: roost_worker::rpi::provider::ProviderError) -> CommandFailure {
    CommandFailure::generic(error.to_string())
}

fn auth_failure(operation: &str, error: &ModelsError) -> CommandFailure {
    if error.code == ModelsErrorCode::Aborted {
        CommandFailure::generic("Rpi authentication cancelled.")
    } else {
        CommandFailure::generic(format!("Rpi {operation} failed ({}); check this machine's Rpi configuration and provider access.", error.code.as_str()))
    }
}

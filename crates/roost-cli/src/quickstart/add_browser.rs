//! `roost add-browser` — mint one browser pairing grant and print the URL that
//! spends it. Called by the crate's dispatcher. Depends on `add_machine` for
//! where the coordinator's database and installed definition are, on
//! `quickstart::grant` for the grant, and on `quickstart::pairing_url` for the
//! URL shape `roost quickstart` opens.
//!
//! It exists for a coordinator no browser has paired with yet and no desktop
//! can open: a container, a VM, a server reached over SSH. The operator runs it
//! where the coordinator's database is reachable and opens the printed URL
//! wherever they have a browser. stdout carries the URL and nothing else.

use std::process::ExitCode;

use clap::Args;
use roost_host::coord_config_loader::{ENV_COORDINATOR_BIND, ENV_WEB_PUBLIC_URL};
use roost_host::{DEFAULT_COORDINATOR_BIND, EnvSource, ProcessEnv, normalize_https_origin};

use crate::command_error::CommandFailure;
use crate::quickstart::add_machine::{
    coordinator_database, installed_coordinator, missing_database_refusal,
};
use crate::quickstart::grant::{GrantKind, mint_host_grant};
use crate::quickstart::pairing_url;
use crate::status::service_definition::{InstalledEnvironment, declared_value};
use crate::wall_clock;

/// The label a grant gets when the operator names none.
const DEFAULT_BROWSER_GRANT_LABEL: &str = "add-browser";

/// `roost add-browser [--label NAME]`.
#[derive(Debug, Args)]
#[command(
    about = "Mint a one-shot browser pairing grant and print the URL that spends it",
    long_about = "Mints a one-shot browser grant and prints the pairing URL. Run it where the \
                  coordinator's database is reachable — on its host, or inside its container. \
                  The URL's origin is ROOST_WEB_PUBLIC_URL when the coordinator declares one, \
                  else the coordinator's loopback port."
)]
pub struct AddBrowserArgs {
    /// The name the grant is recorded under until a browser spends it.
    #[arg(long, value_name = "NAME")]
    pub label: Option<String>,
}

pub async fn run(args: &AddBrowserArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let installed = installed_coordinator(&env, platform);
    let database = coordinator_database(&installed, &env).ok_or_else(missing_database_refusal)?;
    let origin = browser_origin(&installed, &env)?;
    let label = args
        .label
        .clone()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BROWSER_GRANT_LABEL.to_owned());
    if label.chars().any(char::is_control) {
        return Err(CommandFailure::generic(
            "--label must be a single line with no control characters",
        ));
    }
    let grant =
        mint_host_grant(&database, GrantKind::Browser, &label, wall_clock::now_ms()).await?;
    println!("{}", pairing_url(&origin, grant.expose()));
    eprintln!(
        "The grant is one-shot and is accepted for 24 hours. Open the URL in the browser to pair; \
         anyone who opens it first pairs instead."
    );
    Ok(ExitCode::SUCCESS)
}

/// The origin a browser reaches this coordinator at: the declared front door,
/// else loopback on the coordinator's own port.
///
/// The installed definition is read before this shell, for the reason
/// `add_machine::dial_url` gives.
pub fn browser_origin(
    installed: &InstalledEnvironment,
    ambient: &dyn EnvSource,
) -> Result<String, CommandFailure> {
    let declared = |name: &str| {
        declared_value(installed, name)
            .map(str::to_string)
            .or_else(|| ambient.get(name).filter(|value| !value.trim().is_empty()))
    };
    if let Some(front_door) =
        normalize_https_origin(declared(ENV_WEB_PUBLIC_URL).as_deref(), ENV_WEB_PUBLIC_URL)?
    {
        return Ok(front_door);
    }
    let bind =
        declared(ENV_COORDINATOR_BIND).unwrap_or_else(|| DEFAULT_COORDINATOR_BIND.to_owned());
    let port = bind
        .rsplit_once(':')
        .map(|(_, port)| port)
        .filter(|port| port.parse::<u16>().is_ok())
        .ok_or_else(|| {
            CommandFailure::generic(format!(
                "{ENV_COORDINATOR_BIND}={bind} names no port to reach the coordinator on"
            ))
        })?;
    Ok(format!("http://127.0.0.1:{port}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use roost_host::MapEnv;

    use super::browser_origin;
    use crate::status::service_definition::InstalledEnvironment;

    fn installed(entries: &[(&str, &str)]) -> InstalledEnvironment {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_declared_front_door_is_the_origin_and_the_installed_one_wins() {
        let ambient = MapEnv::new().with("ROOST_WEB_PUBLIC_URL", "https://shell.example.com");
        let origin = browser_origin(
            &installed(&[("ROOST_WEB_PUBLIC_URL", "https://roost.example.com/")]),
            &ambient,
        )
        .unwrap();
        assert_eq!(origin, "https://roost.example.com");
        assert_eq!(
            browser_origin(&installed(&[]), &ambient).unwrap(),
            "https://shell.example.com"
        );
    }

    #[test]
    fn with_no_front_door_the_origin_is_loopback_on_the_bind_port() {
        let ambient = MapEnv::new().with("ROOST_COORDINATOR_BIND", "0.0.0.0:4300");
        assert_eq!(
            browser_origin(&installed(&[]), &ambient).unwrap(),
            "http://127.0.0.1:4300"
        );
        assert_eq!(
            browser_origin(&installed(&[]), &MapEnv::new()).unwrap(),
            "http://127.0.0.1:4113"
        );
        assert!(
            browser_origin(
                &installed(&[("ROOST_COORDINATOR_BIND", "no-port")]),
                &MapEnv::new()
            )
            .is_err()
        );
    }
}

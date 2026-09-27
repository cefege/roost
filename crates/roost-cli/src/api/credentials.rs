//! Where `roost api` points and what it presents when it gets there. Called by
//! `api::client` and by `api::device`; depends on the environment, on the
//! installed coordinator definition this crate's `status` already reads, and on
//! the two environment names the deploy group already owns.
//!
//! ONE CREDENTIAL, ONE NAME. v2 read a device key from `~/.roost/cli-key` and
//! signed a JWT with it, redeeming a one-shot grant against a local
//! coordinator database the first time it ran. Signing is an ed25519 operation
//! and `roost-cli` has no signer, so that path is not ported: what `roost api`
//! presents is the bearer this crate already presents for every other
//! coordinator call. A second way to be authenticated would be a second answer
//! to "who is asking", and the coordinator's audit row can only record one.
//!
//! WHY THE INSTALLED DEFINITION IS THE FALLBACK AND NOT THE FIRST CHOICE.
//! `roost status` reads the installed coordinator unit because that unit is
//! what the running coordinator booted with, and it is the only place the
//! answer survives the shell that installed it. An operator who points this
//! command somewhere else says so in `ROOST_COORD_URL`, and that has to win —
//! but a shell that says nothing is asking about the install on this machine,
//! and answering with the install is the only answer that machine can give.

use roost_host::{EnvSource, HostPlatform};

use crate::command_error::CommandFailure;
use crate::deploy::keeper_client::{CLI_TOKEN_ENV, COORD_URL_ENV};
use crate::status::collect::{installed_coordinator_environment, resolve_endpoint};

/// The bearer this command presents, or `None` when the operator enrolled none.
///
/// Read from the environment only. Nothing here opens a key file, and nothing
/// here can put a credential on stdout: the value is handed straight to the
/// client's default headers and is never formatted.
pub fn token(env: &dyn EnvSource) -> Option<String> {
    env.get(CLI_TOKEN_ENV).filter(|value| !value.is_empty())
}

/// The coordinator this command talks to, as an origin.
///
/// `ROOST_COORD_URL` first, then the installed coordinator's own bind. The
/// refusal names both, because an operator whose shell names a URL that is not
/// one and whose unit names nothing else needs to know which of the two was
/// consulted.
pub fn origin(env: &dyn EnvSource, platform: HostPlatform) -> Result<String, CommandFailure> {
    if let Some(declared) = env.get(COORD_URL_ENV).filter(|value| !value.is_empty()) {
        return Ok(declared.trim_end_matches('/').to_string());
    }
    let installed = installed_coordinator_environment(env, platform);
    if let Some(bind) = resolve_endpoint(&installed, None).coord_url {
        return Ok(bind);
    }
    Err(CommandFailure::generic(format!(
        "{COORD_URL_ENV} is empty and this machine has no installed coordinator definition naming \
         a bind; a headless call needs a coordinator to answer"
    )))
}

#[cfg(test)]
mod tests {
    use super::token;
    use crate::deploy::keeper_client::CLI_TOKEN_ENV;
    use roost_host::EnvSource;

    struct FixedEnv(Vec<(String, String)>);

    impl EnvSource for FixedEnv {
        fn get(&self, name: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }

        /// Nothing here is a filesystem, so there is no home to resolve. The
        /// token reader never asks, and a fake that invented one would let a
        /// caller pass a test it would fail in production.
        fn home_dir(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    fn env_with(pairs: &[(&str, &str)]) -> FixedEnv {
        FixedEnv(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        )
    }

    #[test]
    fn an_enrolled_bearer_is_read_from_the_environment() {
        let env = env_with(&[(CLI_TOKEN_ENV, "enrolled-bearer")]);
        assert_eq!(token(&env).as_deref(), Some("enrolled-bearer"));
    }

    #[test]
    fn an_unset_or_cleared_bearer_is_absent_rather_than_empty() {
        assert_eq!(token(&env_with(&[])), None);
        assert_eq!(token(&env_with(&[(CLI_TOKEN_ENV, "")])), None);
    }
}

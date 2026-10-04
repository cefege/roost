//! The linger check a deploy runs on its TARGET, as the account it ssh'd in
//! as. Called by deploy/run.rs before anything is staged; depends on
//! services/linger.rs for the sequence and on deploy/ssh.rs for the transport,
//! so the target is asked exactly what a local install asks itself.

use roost_host::HostPlatform;
use roost_platform::posix_shell_quote;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::ssh;
use crate::services::linger::{CommandAnswer, LingerCommands, LingerOutcome, require_linger};

/// Runs each linger command on one deploy target over the deploy's own ssh
/// transport. Every argument is quoted, so a user name the target reported
/// back cannot become a second command.
#[derive(Debug, Clone, Copy)]
pub struct SshLingerCommands<'host> {
    pub host: &'host str,
}

impl LingerCommands for SshLingerCommands<'_> {
    fn run_command(&mut self, argv: Vec<String>) -> impl Future<Output = CommandAnswer> + Send {
        let host = self.host;
        async move {
            let command = argv
                .iter()
                .map(|part| posix_shell_quote(part))
                .collect::<Vec<_>>()
                .join(" ");
            match ssh::exec(host, &command).await {
                Ok(outcome) => CommandAnswer {
                    succeeded: outcome.ok(),
                    detail: outcome.detail(),
                    stdout: outcome.stdout,
                },
                Err(failure) => CommandAnswer {
                    succeeded: false,
                    stdout: String::new(),
                    detail: failure.message,
                },
            }
        }
    }
}

/// Refuse a Linux target whose user manager stops at logout, turning linger
/// on first when the remote account may. Exit 3: the target cannot keep the
/// release running, which is what `NO_REMOTE_RUNTIME` means.
pub async fn require_target_linger(
    host: &str,
    platform: HostPlatform,
) -> Result<(), CommandFailure> {
    match require_linger(platform, &mut SshLingerCommands { host }).await {
        Ok(LingerOutcome::Enabled { user }) => {
            eprintln!(">> enabled linger for {user} on {host}, so Roost services outlive logout");
            Ok(())
        }
        Ok(LingerOutcome::AlreadyOn { .. } | LingerOutcome::NotApplicable) => Ok(()),
        Err(error) => Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!("{host}: {error}"),
        )),
    }
}

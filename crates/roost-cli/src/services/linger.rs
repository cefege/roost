//! Systemd linger for the account Roost's user units run under: whether that
//! account's user manager outlives its last logout. Called by the local install
//! paths (`quickstart`, `join`), by `roost deploy` over ssh, and by `roost
//! status`; depends only on a [`LingerCommands`] runner, so the whole sequence
//! is driven by a scripted runner in tests and never by a real `loginctl`.
//!
//! Without linger, systemd stops the user manager at the account's last logout
//! and every `--user` unit with it: a machine whose operator logged out lost its
//! worker and a keeper PTY that way. So an install that cannot turn linger on
//! refuses rather than installing services that die at logout.

use std::process::Stdio;
use std::time::Duration;

use roost_host::HostPlatform;
use tracing::{info, warn};

/// How long one `loginctl` or `id` call may take. `loginctl` talks to logind
/// over D-Bus, which can block on a wedged daemon; a check that hangs is worse
/// than one that reports linger as off.
pub const LINGER_COMMAND_DEADLINE: Duration = Duration::from_secs(5);

/// What one command on the host in question answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAnswer {
    /// Whether it ran and exited 0.
    pub succeeded: bool,
    /// Its standard output, for the two commands whose answer is printed.
    pub stdout: String,
    /// What to log when it failed: its stderr, or why it could not run.
    pub detail: String,
}

/// The seam between the linger sequence and the host it asks about: this
/// machine's own process table, a deploy target over ssh, or a test's script.
pub trait LingerCommands {
    /// Run `argv` on the host and report what it answered. Never an `Err`: a
    /// command that could not run is a failed answer, and the sequence decides
    /// what a failure means.
    fn run_command(&mut self, argv: Vec<String>) -> impl Future<Output = CommandAnswer> + Send;
}

/// Linger as `roost status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LingerState {
    /// The account the answer is about.
    pub user: String,
    /// Whether its user manager outlives its last logout.
    pub enabled: bool,
}

/// What [`require_linger`] found, or did, before an install may proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LingerOutcome {
    /// A launchd agent belongs to the GUI login domain; there is no linger to
    /// ask about.
    NotApplicable,
    /// Linger was already on; nothing was changed.
    AlreadyOn { user: String },
    /// Linger was off and this check turned it on.
    Enabled { user: String },
}

/// Why an install must not put services on this host.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LingerError {
    #[error(
        "linger is off for {user}: Roost services stop when you log out. Run: sudo loginctl \
         enable-linger {user}"
    )]
    Off {
        /// The account whose user manager stops at logout.
        user: String,
    },
    #[error(
        "the account Roost's services run as cannot be named, so its linger cannot be checked: {detail}"
    )]
    AccountUnresolved {
        /// What `id -un` answered instead of a name.
        detail: String,
    },
}

/// The command that names the account the units run under.
pub fn account_name_command() -> Vec<String> {
    owned(&["id", "-un"])
}

/// The read: prints `yes` or `no`. It fails outright for an account that is
/// neither logged in nor lingering, which is an answer of "off".
pub fn linger_query_command(user: &str) -> Vec<String> {
    owned(&["loginctl", "show-user", user, "-p", "Linger", "--value"])
}

/// The unprivileged enable. polkit lets an account turn on its own linger
/// from an active session.
pub fn enable_linger_command(user: &str) -> Vec<String> {
    owned(&["loginctl", "enable-linger", user])
}

/// The escalated enable, non-interactive: `-n` makes sudo fail rather than
/// prompt, so an install over ssh can never hang on a password.
pub fn escalated_enable_linger_command(user: &str) -> Vec<String> {
    owned(&["sudo", "-n", "loginctl", "enable-linger", user])
}

/// The account the host's commands run as.
pub async fn account_name<R: LingerCommands>(commands: &mut R) -> Result<String, LingerError> {
    let answer = commands.run_command(account_name_command()).await;
    let name = answer.stdout.trim();
    let usable = !name.is_empty() && !name.chars().any(|ch| ch.is_whitespace() || ch.is_control());
    if answer.succeeded && usable {
        return Ok(name.to_string());
    }
    Err(LingerError::AccountUnresolved {
        detail: if answer.succeeded {
            format!("id -un printed {:?}", answer.stdout)
        } else {
            answer.detail
        },
    })
}

/// Whether `user`'s linger is on. Anything but a successful `yes` is off.
pub async fn linger_is_on<R: LingerCommands>(commands: &mut R, user: &str) -> bool {
    let answer = commands.run_command(linger_query_command(user)).await;
    answer.succeeded && answer.stdout.trim() == "yes"
}

/// Read linger for the account the host's commands run as, changing nothing.
/// `None` on a platform with no linger.
pub async fn linger_state<R: LingerCommands>(
    platform: HostPlatform,
    commands: &mut R,
) -> Option<Result<LingerState, LingerError>> {
    if platform != HostPlatform::Linux {
        return None;
    }
    let user = match account_name(commands).await {
        Ok(user) => user,
        Err(error) => return Some(Err(error)),
    };
    let enabled = linger_is_on(commands, &user).await;
    Some(Ok(LingerState { user, enabled }))
}

/// Make sure the account's units will outlive its logout, turning linger on
/// when it is off: unprivileged first, then `sudo -n`. Linger is re-read after
/// the attempts rather than inferred from their exit codes, because the read
/// is the only answer that says what systemd will actually do at logout.
pub async fn require_linger<R: LingerCommands>(
    platform: HostPlatform,
    commands: &mut R,
) -> Result<LingerOutcome, LingerError> {
    if platform != HostPlatform::Linux {
        return Ok(LingerOutcome::NotApplicable);
    }
    let user = account_name(commands).await?;
    if linger_is_on(commands, &user).await {
        info!(user = %user, "linger already on");
        return Ok(LingerOutcome::AlreadyOn { user });
    }
    let unprivileged = commands.run_command(enable_linger_command(&user)).await;
    if !unprivileged.succeeded {
        warn!(user = %user, detail = %unprivileged.detail, "loginctl enable-linger refused; trying sudo -n");
        let escalated = commands
            .run_command(escalated_enable_linger_command(&user))
            .await;
        if !escalated.succeeded {
            warn!(user = %user, detail = %escalated.detail, "sudo -n loginctl enable-linger refused");
        }
    }
    if linger_is_on(commands, &user).await {
        info!(user = %user, "linger enabled");
        return Ok(LingerOutcome::Enabled { user });
    }
    warn!(user = %user, "linger is off and could not be enabled; refusing to install services");
    Err(LingerError::Off { user })
}

/// The runner for this machine: each command is a child process with a
/// deadline, killed if it overruns.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalLingerCommands;

impl LingerCommands for LocalLingerCommands {
    fn run_command(&mut self, argv: Vec<String>) -> impl Future<Output = CommandAnswer> + Send {
        run_local_command(argv)
    }
}

async fn run_local_command(argv: Vec<String>) -> CommandAnswer {
    let Some((program, arguments)) = argv.split_first() else {
        return failed("an empty command".to_string());
    };
    let child = tokio::process::Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(LINGER_COMMAND_DEADLINE, child).await {
        Err(_) => failed(format!(
            "{program} did not answer within {}s",
            LINGER_COMMAND_DEADLINE.as_secs()
        )),
        Ok(Err(error)) => failed(format!("{program} could not be run: {error}")),
        Ok(Ok(output)) => CommandAnswer {
            succeeded: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
    }
}

fn failed(detail: String) -> CommandAnswer {
    CommandAnswer {
        succeeded: false,
        stdout: String::new(),
        detail,
    }
}

fn owned(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|part| (*part).to_string()).collect()
}

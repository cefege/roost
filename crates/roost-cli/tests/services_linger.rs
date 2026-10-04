//! The linger gate every Linux install path passes before a unit is enabled:
//! an account whose linger is on is left alone, one whose linger is off is
//! turned on when it may be, and one that cannot be turned on refuses the
//! install with the command an operator runs. Driven by a scripted runner,
//! so no test here ever runs `loginctl` or `sudo`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::command_error::GENERIC_FAILURE;
use roost_cli::quickstart::install::require_linger_with;
use roost_cli::services::linger::{
    CommandAnswer, LingerCommands, LingerError, LingerOutcome, account_name_command,
    enable_linger_command, escalated_enable_linger_command, linger_query_command, require_linger,
};
use roost_host::HostPlatform;

const USER: &str = "alice";

/// Answers like a host whose linger starts as `initially_on`, and becomes on
/// when an enable the script allows is run. Records every command it was asked.
struct ScriptedLoginctl {
    linger_on: bool,
    unprivileged_enable_works: bool,
    sudo_enable_works: bool,
    asked: Vec<Vec<String>>,
}

impl ScriptedLoginctl {
    fn new(initially_on: bool, unprivileged_enable_works: bool, sudo_enable_works: bool) -> Self {
        Self {
            linger_on: initially_on,
            unprivileged_enable_works,
            sudo_enable_works,
            asked: Vec::new(),
        }
    }

    fn answer(&mut self, argv: &[String]) -> CommandAnswer {
        let (succeeded, stdout) = if argv == account_name_command().as_slice() {
            (true, format!("{USER}\n"))
        } else if argv == linger_query_command(USER).as_slice() {
            (
                true,
                if self.linger_on { "yes\n" } else { "no\n" }.to_string(),
            )
        } else if argv == enable_linger_command(USER).as_slice() {
            self.linger_on |= self.unprivileged_enable_works;
            (self.unprivileged_enable_works, String::new())
        } else if argv == escalated_enable_linger_command(USER).as_slice() {
            self.linger_on |= self.sudo_enable_works;
            (self.sudo_enable_works, String::new())
        } else {
            panic!("the linger check ran a command it has no business running: {argv:?}");
        };
        CommandAnswer {
            succeeded,
            stdout,
            detail: if succeeded {
                String::new()
            } else {
                "Access denied".to_string()
            },
        }
    }

    fn enables_asked(&self) -> Vec<&Vec<String>> {
        self.asked
            .iter()
            .filter(|argv| argv.iter().any(|part| part == "enable-linger"))
            .collect()
    }
}

impl LingerCommands for ScriptedLoginctl {
    fn run_command(&mut self, argv: Vec<String>) -> impl Future<Output = CommandAnswer> + Send {
        let answer = self.answer(&argv);
        self.asked.push(argv);
        std::future::ready(answer)
    }
}

#[tokio::test]
async fn linger_already_on_makes_no_enable_call() {
    let mut host = ScriptedLoginctl::new(true, true, true);
    let outcome = require_linger(HostPlatform::Linux, &mut host).await;
    assert_eq!(
        outcome,
        Ok(LingerOutcome::AlreadyOn {
            user: USER.to_string()
        })
    );
    assert!(host.enables_asked().is_empty(), "asked: {:?}", host.asked);
    assert!(
        require_linger_with(HostPlatform::Linux, &mut host)
            .await
            .is_ok()
    );
    assert!(host.enables_asked().is_empty(), "asked: {:?}", host.asked);
}

#[tokio::test]
async fn linger_off_and_unprivileged_enable_succeeds_lets_the_install_proceed() {
    let mut host = ScriptedLoginctl::new(false, true, false);
    assert!(
        require_linger_with(HostPlatform::Linux, &mut host)
            .await
            .is_ok()
    );
    assert_eq!(host.enables_asked(), vec![&enable_linger_command(USER)]);
    // Proven by a re-read, not inferred from the enable's exit code.
    assert_eq!(host.asked.last(), Some(&linger_query_command(USER)));
}

#[tokio::test]
async fn linger_off_and_only_sudo_enable_succeeds_lets_the_install_proceed() {
    let mut host = ScriptedLoginctl::new(false, false, true);
    let outcome = require_linger(HostPlatform::Linux, &mut host).await;
    assert_eq!(
        outcome,
        Ok(LingerOutcome::Enabled {
            user: USER.to_string()
        })
    );
    assert_eq!(
        host.enables_asked(),
        vec![
            &enable_linger_command(USER),
            &escalated_enable_linger_command(USER)
        ]
    );
}

#[tokio::test]
async fn linger_off_and_both_enables_failing_refuses_the_install_naming_the_command() {
    let mut host = ScriptedLoginctl::new(false, false, false);
    assert_eq!(
        require_linger(HostPlatform::Linux, &mut host).await,
        Err(LingerError::Off {
            user: USER.to_string()
        })
    );

    let mut host = ScriptedLoginctl::new(false, false, false);
    let refusal = require_linger_with(HostPlatform::Linux, &mut host)
        .await
        .expect_err("an install on a host whose services die at logout is refused");
    assert_eq!(refusal.code, GENERIC_FAILURE);
    assert_eq!(
        refusal.message,
        "linger is off for alice: Roost services stop when you log out. Run: sudo loginctl \
         enable-linger alice"
    );
    assert_eq!(host.enables_asked().len(), 2, "asked: {:?}", host.asked);
}

#[tokio::test]
async fn a_macos_install_never_asks_about_linger() {
    let mut host = ScriptedLoginctl::new(false, false, false);
    assert_eq!(
        require_linger(HostPlatform::MacOs, &mut host).await,
        Ok(LingerOutcome::NotApplicable)
    );
    assert!(host.asked.is_empty(), "asked: {:?}", host.asked);
}

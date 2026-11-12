//! Process-level machine-local Rpi setup boundaries and prompt cancellation.
//! These tests invoke the shipped CLI with isolated worker/home paths and no
//! inherited provider environment; no network request or real credential is used.

use std::path::PathBuf;
use std::process::{Command, Output};

use roost_cli::rpi::interaction::TerminalInteraction;
use rpi_ai::auth::{AuthInteraction, AuthPrompt, ModelsErrorCode};
use tokio_util::sync::CancellationToken;

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-cli-rpi-{label}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("worker/rpi")).unwrap();
        Self { root }
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_roost"))
            .args(["rpi"])
            .args(arguments)
            .env_clear()
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("LOCALAPPDATA", self.root.join("appdata"))
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("ROOST_WORKER_DATA_DIR", self.root.join("worker"))
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.root.join("worker/rpi").join(name), content).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_private_failure(output: &Output, secret: &str) {
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(secret), "stdout exposed configuration");
    assert!(!stderr.contains(secret), "stderr exposed configuration");
    assert!(!stderr.is_empty(), "failure must be visible");
}

#[test]
fn missing_machine_local_setup_lists_no_models_and_explains_login() {
    let scratch = Scratch::new("unconfigured");
    std::fs::create_dir_all(scratch.root.join(".omp")).unwrap();
    std::fs::write(scratch.root.join(".omp/auth.json"), "unrelated malformed data").unwrap();
    let output = scratch.command(&["models"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("No available Rpi chat models"));
    assert!(stdout.contains("roost rpi login <provider>"));
}

#[test]
fn malformed_models_fail_without_echoing_config_values() {
    let scratch = Scratch::new("malformed-models");
    let secret = "private-config-value-not-for-display";
    scratch.write("models.json", &format!("{{\"providers\":{secret}"));
    assert_private_failure(&scratch.command(&["models"]), secret);
}

#[test]
fn malformed_auth_blocks_models_and_logout_without_echoing_credentials() {
    let scratch = Scratch::new("malformed-auth");
    let secret = "private-auth-value-not-for-display";
    scratch.write("auth.json", &format!("{{\"openai\":{{\"key\":\"{secret}\",INVALID"));
    assert_private_failure(&scratch.command(&["models"]), secret);
    assert_private_failure(&scratch.command(&["logout", "openai"]), secret);
}

#[test]
fn unreadable_auth_path_is_not_reported_as_missing_setup() {
    let scratch = Scratch::new("auth-directory");
    std::fs::create_dir(scratch.root.join("worker/rpi/auth.json")).unwrap();
    let output = scratch.command(&["models"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("No available"));
    assert!(!output.stderr.is_empty());
}

#[test]
fn logout_removes_only_this_workers_stored_credential() {
    let scratch = Scratch::new("logout");
    scratch.write("auth.json", r#"{"openai":{"type":"api_key","key":"scratch-not-a-real-key"},"anthropic":{"type":"api_key","key":"keep-scratch-key"}}"#);
    let output = scratch.command(&["logout", "openai"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let auth: serde_json::Value = serde_json::from_slice(&std::fs::read(scratch.root.join("worker/rpi/auth.json")).unwrap()).unwrap();
    assert!(auth.get("openai").is_none());
    assert_eq!(auth["anthropic"]["key"], "keep-scratch-key");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("scratch-not-a-real-key"));
}

#[test]
fn login_refuses_piped_input_without_printing_it_or_saving_credentials() {
    let scratch = Scratch::new("nonterminal");
    let output = scratch.command(&["login", "openai", "--auth", "api-key"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    let auth_path = scratch.root.join("worker/rpi/auth.json");
    if auth_path.exists() {
        let auth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(auth_path).unwrap()).unwrap();
        assert!(auth.as_object().unwrap().is_empty());
    }
}

#[tokio::test]
async fn callback_cancellation_returns_before_acquiring_a_terminal_and_preserves_flow() {
    let flow = CancellationToken::new();
    let prompt = CancellationToken::new();
    prompt.cancel();
    let interaction = TerminalInteraction::new(flow.clone());
    let error = interaction.prompt(AuthPrompt::ManualCode {
        message: "Code".to_owned(),
        placeholder: None,
        signal: Some(prompt),
    }).await.unwrap_err();
    assert_eq!(error.code, ModelsErrorCode::Aborted);
    assert!(!flow.is_cancelled());
}

#[tokio::test]
async fn flow_cancellation_does_not_attempt_to_read_a_secret() {
    let flow = CancellationToken::new();
    flow.cancel();
    let interaction = TerminalInteraction::new(flow);
    let error = interaction.prompt(AuthPrompt::secret("Private key")).await.unwrap_err();
    assert_eq!(error.code, ModelsErrorCode::Aborted);
}

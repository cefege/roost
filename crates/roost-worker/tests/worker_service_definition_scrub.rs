//! Erasing a one-shot authorization from the definition that will outlive it.
//!
//! The value is the hazard. A bootstrap token is one-shot by definition and a
//! keeper force-live retire ends every PTY on the machine, so a value left in a
//! unit file is re-read on every restart with nobody watching — and the
//! definition is the one file a service manager hands the same environment to
//! forever.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use roost_host::{HostPlatform, MapEnv};
use roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV;
use roost_worker::host::install::{
    BOOTSTRAP_TOKEN_ENV, InstallError, ServiceDefinition, scrub_service_definition_env,
};
use scratch::Scratch;

/// A LaunchAgent plist shaped the way `roost-cli`'s installer writes one: each
/// tag on its own line, the environment entries in one dict.
fn launch_agent() -> String {
    [
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
        "<plist version=\"1.0\">",
        "<dict>",
        "  <key>Label</key>",
        "  <string>com.roost.worker-v3</string>",
        "  <key>EnvironmentVariables</key>",
        "  <dict>",
        "    <key>ROOST_BOOTSTRAP_TOKEN</key>",
        "    <string>one-shot-secret</string>",
        "    <key>ROOST_WORKER_LABEL</key>",
        "    <string>worker</string>",
        "  </dict>",
        "  <key>RunAtLoad</key>",
        "  <true/>",
        "</dict>",
        "</plist>",
        "",
    ]
    .join("\n")
}

/// A systemd unit shaped the way `roost-cli`'s installer writes one: one quoted
/// pair per `Environment=` line.
fn unit() -> String {
    [
        "[Unit]",
        "Description=roost worker",
        "",
        "[Service]",
        "Environment=\"ROOST_BOOTSTRAP_TOKEN=one-shot-secret\"",
        "Environment=\"ROOST_WORKER_LABEL=worker\"",
        "ExecStart=/opt/roost/bin/roost worker",
        "",
    ]
    .join("\n")
}

/// The environment that points the definition at `path`. The per-platform
/// override is `roost_host`'s, so a fixture writes into a scratch file rather
/// than a real `~/.config/systemd/user`.
fn environment_over(path: &Path, platform: HostPlatform) -> MapEnv {
    let name = match platform {
        HostPlatform::MacOs => "ROOST_WORKER_PLIST",
        _ => "ROOST_WORKER_UNIT",
    };
    MapEnv::new().with(name, &path.display().to_string())
}

fn written_definition(scratch: &Scratch, name: &str, text: &str, mode: u32) -> PathBuf {
    let path = scratch.path(name);
    std::fs::write(&path, text).expect("the definition is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
        .expect("the definition's mode is set");
    path
}

fn read_definition(path: &Path) -> String {
    std::fs::read_to_string(path).expect("the definition is there")
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the definition is there")
        .permissions()
        .mode()
        & 0o7777
}

/// A redeemed token must not still be sitting in the file the next start will
/// read. The rest of the definition is the service's whole configuration, and an
/// erase that took a neighbour with it would break the service in a way that
/// looks like the token was still needed.
#[tokio::test]
async fn a_redeemed_token_is_gone_from_a_launch_agent_and_the_rest_survives() {
    let scratch = Scratch::new("scrub-plist");
    let path = written_definition(&scratch, "worker.plist", &launch_agent(), 0o644);
    let env = environment_over(&path, HostPlatform::MacOs);

    let removed = scrub_service_definition_env(&env, HostPlatform::MacOs, BOOTSTRAP_TOKEN_ENV)
        .await
        .expect("a LaunchAgent is editable");
    assert!(
        removed,
        "the definition carried the token, so the erase did work"
    );

    let updated = read_definition(&path);
    assert!(
        !updated.contains(BOOTSTRAP_TOKEN_ENV) && !updated.contains("one-shot-secret"),
        "the value is still in the file the next start reads: {updated}"
    );
    for survivor in [
        "com.roost.worker-v3",
        "ROOST_WORKER_LABEL",
        "<key>RunAtLoad</key>",
    ] {
        assert!(
            updated.contains(survivor),
            "{survivor} was in the definition and is not in it any more"
        );
    }
    assert_eq!(
        mode_of(&path),
        0o600,
        "a file a secret was just removed from is not left world-readable"
    );
}

/// The same on Linux, where the line is a quoted pair rather than two tags, and
/// where the edit has to be handed to the service manager or the running unit
/// keeps serving the contents it already read.
#[tokio::test]
async fn a_redeemed_token_is_gone_from_a_unit_and_the_rest_survives() {
    let scratch = Scratch::new("scrub-unit");
    let path = written_definition(&scratch, "worker.service", &unit(), 0o644);
    let env = environment_over(&path, HostPlatform::Linux);

    let removed = scrub_service_definition_env(&env, HostPlatform::Linux, BOOTSTRAP_TOKEN_ENV)
        .await
        .expect("a systemd unit is editable");
    assert!(removed);

    let updated = read_definition(&path);
    assert!(
        !updated.contains("one-shot-secret"),
        "the token is still in the unit: {updated}"
    );
    for survivor in [
        "ROOST_WORKER_LABEL",
        "ExecStart=/opt/roost/bin/roost worker",
        "[Service]",
    ] {
        assert!(
            updated.contains(survivor),
            "{survivor} was in the unit and is not in it any more"
        );
    }
    assert_eq!(
        mode_of(&path),
        0o600,
        "a file a secret was just removed from is not left world-readable"
    );
}

/// systemd packs a list of pairs onto one line, and a line filter would take the
/// whole list with it. `ROOST_KEEPER_FORCE_LIVE_RETIRE` going missing is
/// survivable; `ROOST_COORDINATOR_URL` going missing with it is a worker that has
/// just been pointed at the default coordinator instead of the one its operator
/// chose.
#[tokio::test]
async fn a_pair_sharing_a_line_with_the_erased_one_survives_unchanged() {
    let scratch = Scratch::new("scrub-shared-line");
    let original = [
        "[Service]",
        "Environment=\"ROOST_KEEPER_FORCE_LIVE_RETIRE=1\" \"ROOST_COORDINATOR_URL=https://coord.example\"",
        "",
    ]
    .join("\n");
    let path = written_definition(&scratch, "worker.service", &original, 0o600);
    let env = environment_over(&path, HostPlatform::Linux);

    scrub_service_definition_env(&env, HostPlatform::Linux, KEEPER_FORCE_LIVE_RETIRE_ENV)
        .await
        .expect("a systemd unit is editable");

    let updated = read_definition(&path);
    assert!(
        !updated.contains(KEEPER_FORCE_LIVE_RETIRE_ENV),
        "the authorization is still in the unit: {updated}"
    );
    assert_eq!(
        updated,
        format!("[Service]\nEnvironment=\"ROOST_COORDINATOR_URL=https://coord.example\"\n"),
        "the survivor keeps its own bytes: re-encoding it would turn a value the \
         installer escaped once into a value systemd reads differently"
    );
}

/// A value the installer escaped, on a line where something else is erased. The
/// `\t` and `%%` are the two the installer's own `quoted_value` emits, so this is
/// a byte round-trip through an edit rather than a formatting preference.
#[tokio::test]
async fn an_escaped_value_on_the_edited_line_keeps_its_escapes() {
    let scratch = Scratch::new("scrub-escapes");
    let original = [
        "[Service]",
        "Environment=\"ROOST_KEEPER_FORCE_LIVE_RETIRE=1\" \"ROOST_WORKER_LABEL=a\\tb\"",
        "Environment=\"ROOST_REACHABLE_ADDR=100%\"",
        "",
    ]
    .join("\n");
    let path = written_definition(&scratch, "worker.service", &original, 0o600);
    let env = environment_over(&path, HostPlatform::Linux);

    scrub_service_definition_env(&env, HostPlatform::Linux, KEEPER_FORCE_LIVE_RETIRE_ENV)
        .await
        .expect("a systemd unit is editable");

    assert_eq!(
        read_definition(&path),
        [
            "[Service]",
            "Environment=\"ROOST_WORKER_LABEL=a\\tb\"",
            "Environment=\"ROOST_REACHABLE_ADDR=100%\"",
            "",
        ]
        .join("\n"),
        "an escape the installer wrote must come back out of the erase exactly \
         as it went in, or the next boot reads a different value"
    );
}

/// A definition that never carried the value is left exactly as it was: it is
/// the shape of a value an operator exported in a shell and started the worker
/// by hand, and rewriting a file that is already correct is how a scrub turns
/// into a corruption.
#[tokio::test]
async fn a_definition_that_never_carried_the_value_is_left_alone() {
    let scratch = Scratch::new("scrub-absent");
    let original = unit().replace(BOOTSTRAP_TOKEN_ENV, "ROOST_SOMETHING_ELSE");
    let path = written_definition(&scratch, "worker.service", &original, 0o644);
    let env = environment_over(&path, HostPlatform::Linux);

    let removed = scrub_service_definition_env(&env, HostPlatform::Linux, BOOTSTRAP_TOKEN_ENV)
        .await
        .expect("a systemd unit is readable");
    assert!(
        !removed,
        "erasing nothing is the correct answer, and it says so"
    );
    assert_eq!(
        read_definition(&path),
        original,
        "a file with nothing to remove must come back byte for byte"
    );
    assert_eq!(
        mode_of(&path),
        0o644,
        "and its mode must not change either, because nothing was erased and \
         the installer chose that mode"
    );
}

/// An edit to a systemd unit that is never handed to systemd is an edit the
/// running service does not see: it serves the contents it read at start until
/// something reloads it. A plist needs no such step, and asking launchd for one
/// would be a process spawned to be ignored.
#[test]
fn only_the_definition_that_needs_a_reload_gets_one() {
    assert!(
        ServiceDefinition::SystemdUserUnit.needs_reload(),
        "systemd serves a unit's cached contents until it is reloaded, so an \
         edit nobody hands it does not take effect until the next login"
    );
    assert!(
        !ServiceDefinition::LaunchAgent.needs_reload(),
        "launchd re-reads the plist on the next launch, so the reload would be \
         a process spawned to be ignored"
    );
    assert_eq!(
        ServiceDefinition::for_platform(HostPlatform::Linux),
        Ok(ServiceDefinition::SystemdUserUnit)
    );
    assert_eq!(
        ServiceDefinition::for_platform(HostPlatform::MacOs),
        Ok(ServiceDefinition::LaunchAgent)
    );
}

/// The name is matched against the text of the definition, so a value that is
/// not a plain environment name is refused rather than edited by a rule nobody
/// wrote. A caller passing a path would otherwise be removing whatever the path
/// happened to contain.
#[tokio::test]
async fn a_name_that_is_not_an_environment_name_is_refused() {
    let scratch = Scratch::new("scrub-bad-name");
    let path = written_definition(&scratch, "worker.plist", &launch_agent(), 0o600);
    let env = environment_over(&path, HostPlatform::MacOs);
    for bad in ["lower_case", "1LEADING_DIGIT", "WITH SPACE", "", "A=B"] {
        let refused = scrub_service_definition_env(&env, HostPlatform::MacOs, bad).await;
        assert_eq!(
            refused,
            Err(InstallError::NotAnEnvName {
                key: bad.to_string()
            }),
            "{bad:?} was matched against the definition instead of being refused"
        );
    }
    assert_eq!(
        read_definition(&path),
        launch_agent(),
        "a refused name must not have edited anything"
    );
}

/// A definition that is not there is reported, not swallowed. An operator who
/// spent a one-shot authorization and then edited a file that does not exist has
/// a problem worth naming.
#[tokio::test]
async fn a_definition_that_is_not_there_is_reported_rather_than_assumed_clean() {
    let scratch = Scratch::new("scrub-missing");
    let env = environment_over(&scratch.path("never-installed.plist"), HostPlatform::MacOs);
    let refused = scrub_service_definition_env(&env, HostPlatform::MacOs, BOOTSTRAP_TOKEN_ENV)
        .await
        .expect_err("there is no definition to erase");
    assert!(
        matches!(refused, InstallError::Unreadable { .. }),
        "a missing definition is a fact the caller needs: {refused}"
    );
}

/// v3 ships no Windows worker, so there is no definition to edit and no stub
/// that pretends otherwise. v2 answered `false` here, which reads exactly like a
/// successful erase of a file nobody checked.
#[test]
fn a_platform_with_no_definition_is_refused_rather_than_pretending() {
    assert_eq!(
        ServiceDefinition::for_platform(HostPlatform::Windows),
        Err(InstallError::UnsupportedPlatform { platform: "win32" }),
        "v2 answered false here, which is indistinguishable from an erase that \
         worked on a file that was never checked"
    );
    let refused =
        ServiceDefinition::for_platform(HostPlatform::Windows).expect_err("windows is not ported");
    assert!(
        refused.to_string().contains("not ported"),
        "the refusal says why, not merely that: {refused}"
    );
}

/// The token a first boot redeems and the authorization an operator spends are
/// erased through the same call. If the two names ever drifted, the bootstrap
/// token would be the one left behind in the file.
#[test]
fn the_two_one_shot_names_are_erased_by_the_same_rule() {
    assert_eq!(BOOTSTRAP_TOKEN_ENV, "ROOST_BOOTSTRAP_TOKEN");
    assert_eq!(
        KEEPER_FORCE_LIVE_RETIRE_ENV,
        "ROOST_KEEPER_FORCE_LIVE_RETIRE"
    );
    assert_ne!(
        BOOTSTRAP_TOKEN_ENV, KEEPER_FORCE_LIVE_RETIRE_ENV,
        "two one-shot values sharing a name would erase each other, and neither"
    );
}

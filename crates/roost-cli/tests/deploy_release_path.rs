//! The parts of a deploy that are pure functions: the ssh boundary's shape, the
//! manifest and report pair, the per-key deploy environment rule, the release
//! retirement decision, and the exit-code table.
//!
//! Each of these is a place where being wrong is silent. A report that parses
//! into "settled" because a field was unknown, a prior release removed with a
//! command only a worktree accepts, an identity key that resolves from the
//! deploying shell — none of them fails loudly, and all of them are the defects
//! `docs/FAILURE-INDEX.md` records.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::deploy::codes;
use roost_cli::deploy::facts::{self, RemoteFacts};
use roost_cli::deploy::installed::{
    installed_build_sha, installed_release_dir, launchd_program_argument,
    systemd_working_directory,
};
use roost_cli::deploy::manifest::{ApplyManifest, ApplyOutcome, ApplyReport};
use roost_cli::deploy::release::release_digest;
use roost_cli::deploy::retire::{Retirement, plan_retirement};
use roost_cli::deploy::ssh::{self, REMOTE_PATH_PREFIX, SSH_OPTS, read_remote_file};
use roost_cli::status::service_definition::parse_installed_environment;
use roost_host::HostPlatform;
use roost_protocol::keeper_update::KeeperContractV1;
use serde_json::json;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

/// A remote command is an argv, never a shell string assembled by a caller. `--`
/// before the host is what makes a host that looks like an option impossible to
/// inject, and the command itself is the last element so a caller cannot smuggle
/// flags into it.
#[test]
fn an_ssh_argv_cannot_be_smuggled_into_by_the_host() {
    let argv = ssh::ssh_argv(
        "-oProxyCommand=touch /tmp/pwned",
        "true",
        &["-o", "BatchMode=yes"],
    );
    assert_eq!(argv[0], "ssh");
    let separator = argv.iter().position(|element| element == "--").expect("--");
    assert_eq!(argv[separator + 1], "-oProxyCommand=touch /tmp/pwned");
    assert!(
        argv[separator + 2].ends_with("true"),
        "the remote command is the last element: {argv:?}"
    );
    // Everything before `--` is an option this module chose or the caller passed
    // as an explicit pair; nothing from the host reaches that region.
    // `chunks_exact(2)`, not `windows(2)`: the option region is a run of
    // `-o VALUE` pairs, and `windows` slides by one so it also hands back
    // `(VALUE, "-o")` pairs that are not pairs at all.
    let options = &argv[1..separator];
    assert_eq!(options.len() % 2, 0, "options come in pairs: {argv:?}");
    assert!(
        options.chunks_exact(2).all(|pair| pair[0] == "-o"
            && (SSH_OPTS.contains(&pair[1].as_str()) || pair[1] == "BatchMode=yes")),
        "only the shared options and the caller's explicit ones: {argv:?}"
    );
}

/// A non-interactive ssh login runs no `.zshrc`, so without an explicit PATH the
/// target's own service manager is simply absent and every activation fails for a
/// reason that has nothing to do with the release.
#[test]
fn every_remote_command_carries_an_explicit_path() {
    let argv = ssh::ssh_argv("studio", "true", &[]);
    let command = argv.last().unwrap();
    assert!(command.starts_with(REMOTE_PATH_PREFIX), "got: {command}");
    assert!(command.contains("/usr/bin"), "got: {command}");
}

/// A path the target reports back is quoted before it can reach a command. The
/// three characters are the only ones that let a value forge a second command,
/// and none of them can appear in a path the target's own tools produce.
#[test]
fn a_reported_path_with_a_control_character_is_refused() {
    assert!(ssh::reject_control_characters("home", "/home/studio").is_ok());
    for hostile in [
        "/home/studio\nrm -rf /",
        "/home/studio\r",
        "/home/studio\0x",
    ] {
        let failure = ssh::reject_control_characters("home", hostile)
            .expect_err("a control character must be refused");
        assert_eq!(failure.code, codes::SSH_UNREACHABLE);
    }
    assert!(
        ssh::reject_control_characters("home", "").is_err(),
        "an empty home is a refusal, not a path"
    );
}

/// Reading an absent file on the target is the normal first-install state, so the
/// probe prints nothing rather than failing — and the path is quoted so a home
/// with a space in it is still read.
#[test]
fn reading_a_definition_that_may_not_exist_still_quotes_it() {
    let command = read_remote_file("/home/a b/roost3-worker.service");
    assert!(
        command.contains("'/home/a b/roost3-worker.service'"),
        "got: {command}"
    );
    assert!(command.contains("test -f"), "got: {command}");
}

/// The manifest refuses a value that would become a directory name or a unit
/// line. A build identity is a directory name, so a manifest carrying a path
/// separator in one is a manifest that would install outside the release root.
#[test]
fn a_manifest_refuses_a_build_identity_that_is_not_one() {
    let environment = BTreeMap::new();
    for bad in ["../../etc", "sha/../other", "", "sha with space"] {
        let manifest = ApplyManifest::new(
            bad,
            "/home/studio/.roost-deploy/x",
            &"a".repeat(64),
            environment.clone(),
        );
        assert!(manifest.validate().is_err(), "{bad:?} must be refused");
    }
    let good = ApplyManifest::new(
        "b1d1836a",
        "/home/studio/.roost-deploy/x",
        &"a".repeat(64),
        environment,
    );
    assert!(good.validate().is_ok(), "a real build identity is admitted");
}

/// A report whose outcome this build does not have is a refusal, not a default.
/// The dangerous direction is a report that omits `outcome` and decodes as
/// settled, so `deny_unknown_fields` and the schema check are both load-bearing.
#[test]
fn a_report_from_another_vocabulary_is_refused() {
    let settled = ApplyReport::new(ApplyOutcome::Settled, "worker is running the release");
    let line = format!(
        "{}{}",
        roost_cli::deploy::manifest::REPORT_PREFIX,
        settled.encode().unwrap()
    );
    let decoded = ApplyReport::decode(&format!("some warning\n{line}\n")).unwrap();
    assert_eq!(decoded.outcome, ApplyOutcome::Settled);

    let future = json!({
        "schema": 1,
        "outcome": "settled",
        "detail": "ok",
        "definition_changed": true,
        "definition_path": "/x",
        "release_dir": "/x",
        "settled_by": "some future field"
    });
    let line = format!("{}{future}", roost_cli::deploy::manifest::REPORT_PREFIX);
    assert!(
        ApplyReport::decode(&line).is_err(),
        "an unknown field must be refused, not ignored"
    );

    let other_schema = json!({
        "schema": 99,
        "outcome": "settled",
        "detail": "ok",
        "definition_changed": true,
        "definition_path": "/x"
    });
    let line = format!(
        "{}{other_schema}",
        roost_cli::deploy::manifest::REPORT_PREFIX
    );
    assert!(
        ApplyReport::decode(&line).is_err(),
        "another schema must be refused"
    );

    assert!(
        ApplyReport::decode("the target printed nothing useful").is_err(),
        "silence is not a settled deploy"
    );
}

/// A report with no explanation is a report the deploying box cannot act on, so
/// it is refused rather than shown as an empty success.
#[test]
fn a_report_with_no_explanation_is_refused() {
    let report = json!({
        "schema": 1,
        "outcome": "unsettled",
        "detail": "",
        "definition_changed": false,
        "definition_path": "/x"
    });
    let line = format!("{}{report}", roost_cli::deploy::manifest::REPORT_PREFIX);
    assert!(ApplyReport::decode(&line).is_err());
}

/// A release is proved by hashing its tree, so the digest is a function of the
/// bytes and the names rather than of the order a directory walk produced.
#[test]
fn a_release_digest_is_a_function_of_its_bytes_and_names() {
    let first = tempdir("digest-a");
    let second = tempdir("digest-b");
    for root in [&first, &second] {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("roost"), b"roost bytes").unwrap();
        std::fs::write(bin.join("roost-keeper"), b"keeper bytes").unwrap();
    }
    let one = roost_cli::deploy::release::release_digest(&first.join("bin")).unwrap();
    let two = roost_cli::deploy::release::release_digest(&second.join("bin")).unwrap();
    assert_eq!(one, two, "the same tree in two places has the same digest");

    std::fs::write(second.join("bin").join("roost"), b"different bytes").unwrap();
    let changed = roost_cli::deploy::release::release_digest(&second.join("bin")).unwrap();
    assert_ne!(one, changed, "a changed byte changes the digest");
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

/// A keeper contract a target would be admitted against has to be a contract the
/// protocol accepts, so a staged release reporting nonsense is caught before any
/// keeper is touched.
#[test]
fn a_malformed_keeper_contract_is_refused() {
    let value = json!({
        "protocol_version": 3,
        "supported_features": [],
        "required_features": [],
        "implementation_digest": "a".repeat(64),
        "platform": "linux",
        "arch": "x86_64",
        "build_sha": "b1d1836a"
    });
    assert!(KeeperContractV1::parse(&value).is_ok());
    let mut malformed = value.clone();
    malformed["platform"] = json!("plan9");
    assert!(KeeperContractV1::parse(&malformed).is_err());
    let mut no_digest = value;
    no_digest["implementation_digest"] = json!(null);
    assert!(
        KeeperContractV1::parse(&no_digest).is_ok(),
        "a keeper that cannot name its own binary is a valid contract…"
    );
    assert!(
        no_digest["implementation_digest"].is_null(),
        "…and the absence is what makes every restart against it unproven"
    );
}

fn tempdir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("roost-deploy-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// The staged release is placed where both sides can name it, and it is not
/// inside the install's own release root: a staging directory that collided with
/// a release directory would let a half-written release be run.
#[test]
fn the_staging_directory_is_outside_the_release_root() {
    let staged = roost_cli::deploy::apply_release::staging_dir("/home/studio", "b1d1836a");
    assert_eq!(staged, PathBuf::from("/home/studio/.roost-deploy/b1d1836a"));
    assert!(!staged.starts_with("/home/studio/.local/share"));
}

/// A release is proved against the target, so the manifest's release directory is
/// not something the deploying box chooses: it is the staging path both sides can
/// compute from a home they each already know.
#[test]
fn the_manifest_names_the_staging_path_and_not_an_install_path() {
    let manifest = ApplyManifest::new(
        "b1d1836a",
        "/home/studio/.roost-deploy/b1d1836a",
        &"a".repeat(64),
        BTreeMap::from([(
            ENV_COORDINATOR_URL.to_string(),
            "https://coord.test".to_string(),
        )]),
    );
    let decoded = ApplyManifest::decode(&manifest.encode().unwrap()).unwrap();
    assert_eq!(decoded, manifest);
    assert!(decoded.staged_dir.contains("/.roost-deploy/"));
    assert_eq!(
        decoded
            .environment
            .get(ENV_COORDINATOR_URL)
            .map(String::as_str),
        Some("https://coord.test")
    );
}

/// The remote facts carry the installed program's own path, because a keeper
/// refresh changes no release and therefore has no staged one to name.
#[test]
fn the_facts_name_the_installed_program() {
    let facts = RemoteFacts {
        schema: facts::FACTS_SCHEMA,
        platform: "linux".to_string(),
        home: "/home/studio".to_string(),
        worker_label: "roost3-worker".to_string(),
        definition_path: "/home/studio/.config/systemd/user/roost3-worker.service".to_string(),
        release_root: "/home/studio/.local/share/RoostWorkerV3/service/versions".to_string(),
        service_dir: "/home/studio/.local/share/RoostWorkerV3/service".to_string(),
        installed_program: "/home/studio/.local/share/RoostWorkerV3/service/versions/old/bin/roost"
            .to_string(),
        installed_environment: BTreeMap::new(),
    };
    let decoded = facts::decode(&format!(
        "{}{}",
        facts::FACTS_PREFIX,
        facts::encode(&facts).unwrap()
    ))
    .unwrap();
    assert_eq!(decoded, facts);
}

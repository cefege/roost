//! The checks a self-update makes before it changes anything on the machine: the
//! tag it was asked for, the release's own claim about what it is, and whether
//! the keeper the release ships can take over from the one already running.

use std::path::Path;
use std::process::Command;

use roost_host::{DEV_BUILD_STAMP, GIT_SHA_ENV, ROOST_GIT_SHA_ENV};
use roost_protocol::keeper_update::{KeeperContractV1, keeper_contracts_protocol_compatible};
use serde_json::Value;

use crate::command_error::CommandFailure;

/// Only `v`-prefixed release tags move a worker. `dev` is a source checkout and a
/// bare commit is not a release, so a worker asked for either would retry a
/// download that cannot exist.
pub fn check_tag(tag: &str) -> Result<(), String> {
    if roost_protocol::fleet_update::is_release_tag(tag) {
        Ok(())
    } else {
        Err(format!(
            "`{tag}` is not a release tag; expected a v-prefixed tag such as v3.0.0-rc.37"
        ))
    }
}

/// The staged `roost` binary's own account of itself: its artifact version and
/// its build commit. Both come from the bytes about to be installed, so a tag
/// that does not match its binary stops here rather than on the machine.
pub fn staged_identity(program: &Path) -> Result<(String, String), CommandFailure> {
    let version = run_staged(program, &["version"])?;
    let commit = run_staged(program, &["version", "--build"])?;
    Ok((version, commit))
}

fn run_staged(program: &Path, args: &[&str]) -> Result<String, CommandFailure> {
    // A commit from the shell would be read by a source build, so the probe sees
    // only what the staged binary carries.
    let output = Command::new(program)
        .args(args)
        .env_remove(GIT_SHA_ENV)
        .env_remove(ROOST_GIT_SHA_ENV)
        .output()
        .map_err(|error| {
            CommandFailure::generic(format!(
                "cannot run the staged {}: {error}",
                program.display()
            ))
        })?;
    if !output.status.success() {
        return Err(CommandFailure::generic(format!(
            "the staged {} exited with {} for `{}`",
            program.display(),
            output.status,
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether the staged binary is the release it was fetched as.
pub fn check_identity(tag: &str, version: &str, commit: &str) -> Result<(), String> {
    if version == DEV_BUILD_STAMP {
        return Err(format!(
            "the staged roost for {tag} is an unstamped source build"
        ));
    }
    if version != tag {
        return Err(format!(
            "the release fetched as {tag} reports itself as `{version}`"
        ));
    }
    if commit.is_empty() || commit == "dev" || commit.contains(char::is_whitespace) {
        return Err(format!(
            "the release {tag} carries no build commit (`{commit}`)"
        ));
    }
    Ok(())
}

/// Whether the staged keeper can take over from the running one. Protocol
/// version and required features must agree; the digest and build commit may
/// differ, because a keeper that speaks the same protocol is the one a live
/// session already belongs to. A mismatch is refused here rather than booted,
/// since a worker whose keeper cannot be adopted refuses to start while
/// channels are open.
pub fn check_keeper(staged: &str, running: &str) -> Result<(), String> {
    let target = parse_contract(staged, "the staged keeper")?;
    let current = parse_contract(running, "the running keeper")?;
    if keeper_contracts_protocol_compatible(&target, &current) {
        return Ok(());
    }
    Err(format!(
        "the staged keeper (protocol {}) cannot take over from the running keeper (protocol {}); \
         run `roost deploy` so the keeper is refreshed under the coordinator's fence",
        target.protocol_version, current.protocol_version
    ))
}

fn parse_contract(text: &str, what: &str) -> Result<KeeperContractV1, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("{what} contract is not JSON: {error}"))?;
    KeeperContractV1::parse(&value)
        .map_err(|error| format!("{what} contract is not a contract: {error}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn contract(protocol: u32, required: &[&str], supported: &[&str]) -> String {
        json!({
            "protocol_version": protocol,
            "supported_features": supported,
            "required_features": required,
            "implementation_digest": null,
            "bun_abi": "test-abi",
            "platform": "linux",
            "arch": "x64",
            "build_sha": "abc123",
        })
        .to_string()
    }

    #[test]
    fn only_v_prefixed_release_tags_are_accepted() {
        assert!(check_tag("v3.0.0-rc.37").is_ok());
        assert!(check_tag("v3_1").is_ok());
        assert!(check_tag("dev").is_err());
        assert!(check_tag("v").is_err());
        assert!(check_tag("0123456789abcdef0123456789abcdef01234567").is_err());
        assert!(check_tag("v3/../x").is_err());
        assert!(check_tag("v3 0").is_err());
    }

    #[test]
    fn a_binary_must_report_the_tag_it_was_fetched_as() {
        assert!(check_identity("v3.0.0-rc.37", "v3.0.0-rc.37", "abc123").is_ok());
        let other = check_identity("v3.0.0-rc.37", "v3.0.0-rc.36", "abc123")
            .err()
            .unwrap_or_default();
        assert!(
            other.contains("reports itself as `v3.0.0-rc.36`"),
            "{other}"
        );
        let unstamped = check_identity("v3.0.0-rc.37", "dev", "abc123")
            .err()
            .unwrap_or_default();
        assert!(unstamped.contains("unstamped source build"), "{unstamped}");
        let no_commit = check_identity("v3.0.0-rc.37", "v3.0.0-rc.37", "")
            .err()
            .unwrap_or_default();
        assert!(no_commit.contains("no build commit"), "{no_commit}");
        let dev_commit = check_identity("v3.0.0-rc.37", "v3.0.0-rc.37", "dev")
            .err()
            .unwrap_or_default();
        assert!(dev_commit.contains("no build commit"), "{dev_commit}");
    }

    #[test]
    fn a_keeper_with_the_same_protocol_is_adopted() {
        let running = contract(3, &["pty"], &["pty", "resize"]);
        let staged = contract(3, &["pty"], &["extra", "pty", "resize"]);
        assert!(check_keeper(&staged, &running).is_ok());
    }

    #[test]
    fn a_keeper_with_another_protocol_is_refused() {
        let running = contract(3, &["pty"], &["pty"]);
        let staged = contract(4, &["pty"], &["pty"]);
        let refusal = check_keeper(&staged, &running).err().unwrap_or_default();
        assert!(refusal.contains("roost deploy"), "{refusal}");
    }

    #[test]
    fn a_required_feature_the_running_keeper_lacks_is_refused() {
        let running = contract(3, &[], &["pty"]);
        let staged = contract(3, &["new-thing", "pty"], &["new-thing", "pty"]);
        let refusal = check_keeper(&staged, &running).err().unwrap_or_default();
        assert!(refusal.contains("roost deploy"), "{refusal}");
    }

    #[test]
    fn a_keeper_contract_that_is_not_json_is_refused() {
        assert!(check_keeper("not json", &contract(3, &[], &[])).is_err());
    }
}

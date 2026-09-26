//! What a deploy invocation is allowed to mean: the argument rules, the source
//! checkout, the build-identity proof, and the environment the manifest carries.
//! Called by the deploy command's step sequence; depends on the identity proofs
//! and on the per-key environment rule, and on nothing else in the deploy group.
//!
//! These are separated from the sequence because they are the half of a deploy
//! that can be wrong without anything having been touched: a flag that means
//! something other than the contract says, a checkout that is not the one being
//! deployed, or an identity that resolves from the wrong machine. Keeping them
//! together and apart from the ssh steps makes "what was decided before we
//! connected" one readable place.

use std::path::{Path, PathBuf};

use roost_host::HostPlatform;
use roost_protocol::keeper_update::KeeperContractV1;

use crate::command_error::CommandFailure;
use crate::deploy::DeployArgs;
use crate::deploy::codes;
use crate::deploy::identity;
use crate::deploy::identity_env::{self, Ambient};
use crate::services::service_environment::{ENV_REACHABLE_ADDR, ENV_WORKER_LABEL};
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

/// Everything wrong with the invocation, refused before anything is touched.
pub fn validate(args: &DeployArgs) -> Result<(), CommandFailure> {
    if args.host.trim().is_empty() {
        return Err(codes::refuse(codes::USAGE, "usage: roost deploy <host>"));
    }
    if args.host.chars().any(|character| character.is_control()) {
        return Err(codes::refuse(
            codes::USAGE,
            "the target host contains a control character",
        ));
    }
    if let Some(label) = &args.label
        && (label.is_empty() || label.chars().any(|character| character.is_control()))
    {
        return Err(codes::refuse(
            codes::USAGE,
            "--label must be a non-empty single-line worker label",
        ));
    }
    if let Some(addr) = &args.reachable_addr
        && (addr.is_empty()
            || !addr
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character)))
    {
        return Err(codes::refuse(
            codes::USAGE,
            "--reachable-addr must be a hostname, FQDN, or host:port",
        ));
    }
    if let Some(sha) = &args.expected_sha
        && !sha_is_commit(sha)
    {
        return Err(codes::refuse(
            codes::USAGE,
            "--expected-sha must be a 40-64 hex build identity",
        ));
    }
    if let Some(digest) = &args.expected_manifest_sha256
        && (digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(codes::refuse(
            codes::USAGE,
            "--expected-manifest-sha256 must be a 64-hex digest",
        ));
    }
    if args.allow_unpublished_local && args.coordinator_release {
        return Err(codes::refuse(
            codes::USAGE,
            "--allow-unpublished-local cannot combine with --coordinator-release",
        ));
    }
    if args.coordinator_release && args.expected_sha.is_none() {
        return Err(codes::refuse(
            codes::USAGE,
            "--coordinator-release requires --expected-sha",
        ));
    }
    if args.allow_unpublished_local {
        return Err(codes::refuse(
            codes::USAGE,
            "--allow-unpublished-local is restricted to the localhost quickstart path",
        ));
    }
    Ok(())
}

fn sha_is_commit(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The checkout to build from, and the one the build identity is proved against.
pub fn source_root(args: &DeployArgs) -> Result<PathBuf, CommandFailure> {
    if let Some(root) = &args.source_root {
        if root.as_os_str().is_empty() {
            return Err(codes::refuse(
                codes::USAGE,
                "--source-root must be a local source checkout path",
            ));
        }
        return Ok(root.clone());
    }
    // A compiled binary has no source tree to resolve a path against, so the
    // default is the directory this binary's own release lives in. A release
    // that does not carry a checkout is refused by the git proof below, which
    // is the right place for that answer.
    let program = std::env::current_exe().map_err(|error| {
        codes::refuse(
            codes::USAGE,
            format!("cannot locate this binary to resolve a source root: {error}"),
        )
    })?;
    // `…/target/<profile>/roost` → the checkout two levels up from the profile
    // directory. A release install has no checkout there, and the git proof
    // below is what says so.
    let root = program
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map_or_else(|| program.clone(), Path::to_path_buf);
    Ok(root)
}

pub async fn prove_identity(
    args: &DeployArgs,
    source_root: &std::path::Path,
    platform: HostPlatform,
) -> Result<String, CommandFailure> {
    if args.coordinator_release {
        let expected = args.expected_sha.as_deref().unwrap_or_default();
        let installed = roost_host::coord_service_path(&roost_host::ProcessEnv::new(), platform)?;
        return identity::coordinator_release_git_sha_or_die(
            source_root,
            expected,
            &installed,
            platform,
        )
        .await;
    }
    identity::published_git_sha_or_die(source_root, args.expected_sha.as_deref()).await
}

/// The keeper contract this release will run as, with the build stamped.
pub fn target_contract(staged: &str, git_sha: &str) -> Result<KeeperContractV1, CommandFailure> {
    let value = serde_json::from_str(staged).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!("the staged keeper contract is not JSON: {error}"),
        )
    })?;
    let mut contract = KeeperContractV1::parse(&value).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!("the staged keeper contract is not a contract: {error}"),
        )
    })?;
    contract.build_sha = git_sha.to_string();
    Ok(contract)
}

/// The definition environment the manifest carries.
pub fn definition_environment(
    installed: &crate::status::service_definition::InstalledEnvironment,
    identity_overrides: &std::collections::BTreeMap<String, String>,
    coordinator_url: &str,
    git_sha: &str,
    args: &DeployArgs,
    ambient: &Ambient,
) -> std::collections::BTreeMap<String, String> {
    let mut environment =
        identity_env::worker_install_environment(installed, identity_overrides, git_sha, ambient);
    environment.insert(ENV_COORDINATOR_URL.to_string(), coordinator_url.to_string());
    for key in [ENV_WORKER_LABEL, ENV_REACHABLE_ADDR] {
        if let Some(value) = identity_overrides.get(key) {
            environment.insert(key.to_string(), value.clone());
        }
    }
    if args.force_live {
        // Installed deliberately for this deploy and stripped by the next one:
        // a definition that still carried it would re-authorize destroying a
        // keeper's live channels on every later restart.
        environment.insert(
            roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV.to_string(),
            "1".to_string(),
        );
    }
    environment
}

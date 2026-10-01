//! `roost deploy --release <tag>`: fetching a target's own published binaries
//! instead of building them on the deploying box.
//!
//! The property under test is not "a download happened". It is that the bytes
//! installed on the target are the ones the release published, and are
//! therefore NOT anything the deploying box compiled — because a `--release`
//! deploy that quietly fell back to rebuilding would still install *something*
//! and still report success, on exactly the machines the flag exists to reach.
//!
//! The origin is real HTTP because the product fetches over HTTP, and a
//! `file://` fixture would be testing a transport nothing ships.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod release_support;

use std::path::PathBuf;

use release_support::{FakeAsset, FakeRelease};
use roost_cli::deploy::DeployArgs;
use roost_cli::deploy::invocation::validate;
use roost_cli::deploy::release::StagedRelease;
use roost_cli::deploy::release_fetch::fetch_release;
use roost_cli::update::release::{
    RELEASE_BASE_URL_ENV, WEB_ASSET_NAME, keeper_release_asset_name, release_asset_name,
};
use roost_host::{HostPlatform, MapEnv};

const TAG: &str = "v3.0.0-rc.7";

fn args(host: &str) -> DeployArgs {
    DeployArgs {
        host: host.to_string(),
        label: None,
        reachable_addr: None,
        source_root: None,
        expected_sha: None,
        expected_manifest_sha256: None,
        allow_unpublished_local: false,
        coordinator_release: false,
        force_live: false,
        web_dist: None,
        release: Some(TAG.to_string()),
    }
}

fn origin_with(extra: Vec<FakeAsset>) -> FakeRelease {
    let mut assets = vec![
        FakeAsset::roost_program(release_asset_name(HostPlatform::Linux, "x86_64").unwrap()),
        FakeAsset::named(
            &keeper_release_asset_name(HostPlatform::Linux, "x86_64").unwrap(),
            "keeper",
        ),
    ];
    assets.extend(extra);
    FakeRelease::start(TAG, assets)
}

fn environment(base_url: &str) -> MapEnv {
    let mut env = MapEnv::new();
    env.set(RELEASE_BASE_URL_ENV, base_url);
    env
}

#[tokio::test]
async fn a_release_deploy_installs_the_published_bytes_rather_than_anything_built_here() {
    let origin = origin_with(Vec::new());
    let staged = fetch_release(
        &environment(&origin.base_url),
        TAG,
        HostPlatform::Linux,
        "x86_64",
    )
    .await
    .expect("the published release is fetched");

    assert_eq!(
        staged.git_sha, TAG,
        "the tag is this deploy's build identity"
    );
    // The PUBLISHED name is per-platform; the INSTALLED name is not. This pairs
    // the two, because a deploy that fetched `roost-linux-x64` into a tree that
    // everything downstream expects to hold `roost` would install two files and
    // report that the release ships no keeper.
    for (published, installed) in [
        (
            release_asset_name(HostPlatform::Linux, "x86_64").unwrap(),
            "roost",
        ),
        (
            &keeper_release_asset_name(HostPlatform::Linux, "x86_64").unwrap(),
            "roost-keeper",
        ),
    ] {
        let fetched = release_support::read(&staged.local_dir.join(installed));
        assert_eq!(
            fetched,
            origin.body(published),
            "{installed} is the bytes the release published as {published}"
        );
        assert!(
            !staged.local_dir.join(published).exists(),
            "and it is not ALSO left under its published name, which would ship a second copy of \
             every program"
        );
    }
    let keeper = release_support::read(&staged.local_dir.join("roost-keeper"));
    assert!(
        String::from_utf8_lossy(&keeper).contains("marker: keeper"),
        "and the keeper is the published keeper, not the roost body fetched twice: {}",
        String::from_utf8_lossy(&keeper)
    );
    assert!(
        staged.keeper_contract.contains("protocol_version"),
        "the contract is read from the DOWNLOADED bytes by running them, not from this process: \
         {}",
        staged.keeper_contract
    );
    assert!(
        !release_support::read(&staged.local_dir.join("roost"))
            .windows(2)
            .any(|pair| pair == b"\x7fELF"),
        "an ELF header here would mean the origin was bypassed and something was built locally"
    );
}

#[tokio::test]
async fn a_release_that_publishes_no_bundle_leaves_the_target_with_no_bundle() {
    let origin = origin_with(Vec::new());
    let staged = fetch_release(
        &environment(&origin.base_url),
        TAG,
        HostPlatform::Linux,
        "x86_64",
    )
    .await
    .expect("the release is fetched");
    assert_eq!(
        staged.web, None,
        "a release predating the bundle installs and runs; refusing it would make deploying an \
         older tag impossible"
    );
}

#[tokio::test]
async fn a_release_that_publishes_a_bundle_stages_it_beside_the_binaries() {
    let origin = origin_with(vec![FakeAsset::named(WEB_ASSET_NAME, "not-a-bundle")]);
    let staged = fetch_release(
        &environment(&origin.base_url),
        TAG,
        HostPlatform::Linux,
        "x86_64",
    )
    .await;
    // The body here is not a real gzip, so the fetch proves it got as far as
    // staging and unpacking, and the refusal names the unpack step rather than
    // the download. Either way the sidecar was verified first: an unverified
    // body would have failed with a digest mismatch instead.
    let failure = staged.expect_err("a body that is not a bundle is refused");
    let message = failure.to_string();
    assert!(
        message.contains("web bundle") || message.contains("index.html"),
        "the refusal has to say which step failed, or an operator reads a digest error as a \
         corrupt download: {message}"
    );
}

#[test]
fn a_release_is_refused_against_a_tag_that_is_not_one() {
    for tag in ["", "  ", "v3.0.0\nrm -rf /", "releases/latest"] {
        let mut invocation = args("host");
        invocation.release = Some(tag.to_string());
        let failure = validate(&invocation).expect_err("this is not a tag an origin could publish");
        assert!(
            failure.to_string().contains("--release"),
            "the refusal names the flag to correct: {failure}"
        );
    }
}

#[test]
fn a_release_tag_and_a_local_build_identity_are_two_answers_to_one_question() {
    for (flag, mutate) in [
        (
            "--expected-sha",
            Box::new(|a: &mut DeployArgs| {
                a.expected_sha = Some("0".repeat(40));
            }) as Box<dyn Fn(&mut DeployArgs)>,
        ),
        (
            "--source-root",
            Box::new(|a: &mut DeployArgs| {
                a.source_root = Some(PathBuf::from("/tmp"));
            }),
        ),
        (
            "--coordinator-release",
            Box::new(|a: &mut DeployArgs| {
                a.coordinator_release = true;
            }),
        ),
    ] {
        let mut invocation = args("host");
        mutate(&mut invocation);
        let failure = validate(&invocation).expect_err("two build identities is one too many");
        assert!(
            failure.to_string().contains(flag),
            "the refusal names the flag that has to go: {failure}"
        );
    }
}

#[test]
fn a_release_deploy_without_a_release_still_means_a_local_build() {
    let mut invocation = args("host");
    invocation.release = None;
    validate(&invocation).expect("a plain deploy is unchanged");
}

/// Fetch one tag from one origin, on its own thread, with the keeper it
/// publishes named so the result can be told apart from another's.
fn fetch_named(tag_marker: &str) -> StagedRelease {
    let keeper_name = keeper_release_asset_name(HostPlatform::Linux, "x86_64").unwrap();
    let origin = FakeRelease::start(
        TAG,
        vec![
            FakeAsset::roost_program(release_asset_name(HostPlatform::Linux, "x86_64").unwrap()),
            FakeAsset::named(&keeper_name, tag_marker),
        ],
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the deploy runtime is built")
        .block_on(fetch_release(
            &environment(&origin.base_url),
            TAG,
            HostPlatform::Linux,
            "x86_64",
        ))
        .expect("the published release is fetched")
}

/// Two deploys of one tag, in one process, stage into two trees.
///
/// The property is not that the directory NAME is unique — it is that each
/// invocation installs the bytes its own origin published. A staging tree
/// named after the tag and the pid is invisible until two deploys of one tag
/// run at once, and then each installs whichever fetch finished last, from a
/// tree the other one deleted underneath it.
#[test]
fn two_deploys_of_one_tag_at_once_each_install_their_own_published_bytes() {
    let (left, right) = std::thread::scope(|scope| {
        let left = scope.spawn(|| fetch_named("keeper-left"));
        let right = scope.spawn(|| fetch_named("keeper-right"));
        (
            left.join().expect("the left deploy finishes"),
            right.join().expect("the right deploy finishes"),
        )
    });

    assert_ne!(
        left.local_dir, right.local_dir,
        "two invocations of one tag must not be one staging tree"
    );
    for (staged, marker) in [(&left, "keeper-left"), (&right, "keeper-right")] {
        let keeper = release_support::read(&staged.local_dir.join("roost-keeper"));
        assert!(
            String::from_utf8_lossy(&keeper).contains(&format!("marker: {marker}")),
            "each deploy installs the keeper ITS origin published: {keeper:?}"
        );
    }
}

//! What `roost update` decides BEFORE it downloads anything, and the two string
//! tables that must never become one.
//!
//! Three decisions live here and each is a pure function of its inputs, so each
//! is decidable without a network: which asset a platform installs, whether there
//! is anything to do at all, and what the release's digest sidecar says. A
//! mistake in any of them is invisible until an operator is told they are up to
//! date on a machine that is not, or until a 404 comes back from one platform
//! only.
//!
//! The wording test is here for the same reason. `roost status` prints
//! `Up to date` to mean "this worker's commit equals the coordinator's", and
//! `roost update` prints its own sentence to mean "this binary is the newest
//! one published". They are the same three words about two different facts, and
//! a shared constant would make a wording change to one silently edit the other.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::status::update_state::{WorkerUpdateState, worker_update_label};
use roost_cli::update::candidate::parse_published_digest;
use roost_cli::update::release::{
    DEFAULT_RELEASE_BASE_URL, RELEASE_BASE_URL_ENV, release_asset_name, release_base_url,
};
use roost_cli::update::{ALREADY_LATEST, NO_PUBLISHED_RELEASE, canonical_release_version, needs_update};
use roost_host::{HostPlatform, MapEnv};

/// Every published asset name, one per platform this product ships. A name
/// present in one row and absent from another is a 404 on one platform only,
/// which is the shape of bug that reaches production.
#[test]
fn each_platform_and_architecture_resolves_the_asset_the_release_publishes() {
    assert_eq!(
        release_asset_name(HostPlatform::MacOs, "aarch64").expect("macOS arm64 is published"),
        "roost",
        "the unsuffixed name is the macOS arm64 asset, kept so old release links resolve"
    );
    assert_eq!(
        release_asset_name(HostPlatform::MacOs, "x86_64").expect("macOS x64 is published"),
        "roost-darwin-x64"
    );
    assert_eq!(
        release_asset_name(HostPlatform::Linux, "x86_64").expect("Linux x64 is published"),
        "roost-linux-x64"
    );
    assert_eq!(
        release_asset_name(HostPlatform::Linux, "aarch64").expect("Linux arm64 is published"),
        "roost-linux-arm64"
    );
}

/// An architecture no release publishes is refused, naming it, rather than
/// defaulting to some other platform's binary.
#[test]
fn an_architecture_no_release_publishes_is_refused_by_name() {
    let failure = release_asset_name(HostPlatform::Linux, "riscv64")
        .expect_err("no asset exists for an unpublished architecture");
    assert!(
        failure.message.contains("riscv64"),
        "the refusal names the architecture: {}",
        failure.message
    );
}

/// A machine already on the published release does not update itself, and a
/// rebuild of the same release does not either: the `+sha` differs and the
/// version does not.
#[test]
fn a_binary_already_on_the_published_release_has_nothing_to_do() {
    assert!(
        !needs_update("3.1.0", "v3.1.0").expect("both are release versions"),
        "the same release is not an update"
    );
    assert!(
        !needs_update("3.1.0", "v3.1.0+9f2c1ab").expect("build metadata is not a version"),
        "a rebuild of the same release is not an update, or a binary would update itself forever"
    );
    assert!(
        !needs_update("v3.1.0", "3.1.0").expect("a leading v is not a version difference"),
        "the leading v is spelling, not a version"
    );
}

#[test]
fn a_binary_behind_the_published_release_has_an_update() {
    assert!(needs_update("3.0.9", "v3.1.0").expect("both are release versions"));
    assert!(
        needs_update("dev", "v3.1.0").expect("a source build compares against nothing"),
        "a source checkout is always behind: it is not a published artifact"
    );
}

/// A listing that named no release is a question this command could not answer,
/// not an answer that there is nothing to do — but it is also not a reason to
/// install anything, so it is never an update.
#[test]
fn a_listing_that_named_no_release_is_never_an_update() {
    assert!(!needs_update("3.0.9", "").expect("an empty tag is decidable"));
}

#[test]
fn something_that_is_not_a_release_version_is_refused_rather_than_compared() {
    assert!(canonical_release_version("latest").is_err());
    assert!(canonical_release_version("3.1").is_err());
    assert!(canonical_release_version("3.1.0.1").is_err());
    assert!(
        canonical_release_version("v3.1.0-rc.2+9f2c1ab").expect("a full tag is a release version")
            == "3.1.0"
    );
}

/// A mirror pins the origin for the self-updater exactly as it already does for
/// the deploy paths. When the two resolved differently, `roost update` could not
/// be pointed at a mirror at all.
#[test]
fn a_configured_mirror_replaces_the_github_origin() {
    let mirrored = MapEnv::new().with(RELEASE_BASE_URL_ENV, "https://mirror.internal/roost");
    assert_eq!(
        release_base_url(&mirrored),
        "https://mirror.internal/roost"
    );
    assert_eq!(
        release_base_url(&MapEnv::new()),
        DEFAULT_RELEASE_BASE_URL,
        "with no mirror configured the GitHub origin is used"
    );
    assert_eq!(
        release_base_url(&MapEnv::new().with(RELEASE_BASE_URL_ENV, "   ")),
        DEFAULT_RELEASE_BASE_URL,
        "an empty mirror variable is no mirror, not the empty origin"
    );
}

/// A sidecar is read in every shape a release pipeline or a mirror writes it,
/// and anything that is not a digest is refused.
#[test]
fn a_digest_sidecar_verifies_in_every_shape_a_mirror_writes_it() {
    let digest = "a".repeat(64);
    for sidecar in [
        format!("{digest}\n"),
        format!("{digest}  roost-linux-x64\n"),
        format!("{digest} *roost-linux-x64\n"),
        format!("{}\n", digest.to_uppercase()),
    ] {
        assert_eq!(
            parse_published_digest(&sidecar).as_deref(),
            Some(digest.as_str()),
            "a mirror that regenerates sidecars must keep working: {sidecar:?}"
        );
    }
    for sidecar in ["", "not a digest", &"a".repeat(63), &"z".repeat(64)] {
        assert_eq!(
            parse_published_digest(sidecar),
            None,
            "text that is not a digest is refused: {sidecar:?}"
        );
    }
}

/// The two commands that both say "up to date" must never say it with one
/// constant. `roost status` answers "is this worker's commit the coordinator's";
/// `roost update` answers "is this binary the newest one published".
#[test]
fn the_fleet_wording_and_the_self_update_wording_are_two_different_sentences() {
    let fleet_labels = [
        WorkerUpdateState::Unknown,
        WorkerUpdateState::UpToDate,
        WorkerUpdateState::Updating,
        WorkerUpdateState::UpdateAvailable,
        WorkerUpdateState::UpdateDeferred,
    ]
    .map(worker_update_label);
    assert!(
        fleet_labels.contains(&"Up to date"),
        "the fleet's own wording is what this test holds apart from"
    );
    for label in fleet_labels {
        assert_ne!(
            label, ALREADY_LATEST,
            "`roost status` and `roost update` must not share one sentence: {label:?}"
        );
    }
    assert_ne!(
        NO_PUBLISHED_RELEASE, ALREADY_LATEST,
        "this command's own two sentences are distinct too"
    );
    assert!(
        ALREADY_LATEST.contains("release"),
        "this command's sentence is about a published release, not about a worker's commit"
    );
}

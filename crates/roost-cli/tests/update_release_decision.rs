//! Release selection, channel precedence, asset identity and digest parsing.
//! Exercises the updater's pure decisions before download or activation.
//! The installer suite covers the corresponding executable shell path.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::update::candidate::parse_published_digest;
use roost_cli::update::release::{
    RELEASE_BASE_URL_ENV, RELEASE_DOWNLOAD_ORIGIN, release_asset_name, release_base_url,
};
use roost_cli::update::release_listing::{compare_status_allows, installable_tags};
use roost_cli::update::version::{
    RELEASE_CHANNEL_ENV, ReleaseChannel, canonical_release_version, needs_update, release_channel,
};
use roost_host::{HostPlatform, MapEnv};
use serde_json::json;

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
    assert!(
        needs_update("v3.0.0-rc.3", "v3.0.0-rc.4").expect("both are release versions"),
        "two candidates of one version are two releases"
    );
    assert!(
        needs_update("v3.0.0-rc.4", "v3.0.0").expect("both are release versions"),
        "the final release replaces its last candidate"
    );
    assert!(
        !needs_update("v3.0.0-rc.4", "v3.0.0-rc.4+9f2c1ab").expect("both are release versions"),
        "a rebuild of the same candidate is not an update"
    );
}

/// A listing that named no release is a question this command could not answer,
/// not an answer that there is nothing to do — but it is also not a reason to
/// install anything, so it is never an update.
#[test]
fn a_listing_that_named_no_release_is_never_an_update() {
    assert!(!needs_update("3.0.9", "").expect("an empty tag is decidable"));
}

/// Only a strictly newer release is an update. The listing is ordered by
/// publish date, so a candidate published after its final, or a re-cut older
/// tag, would otherwise move a machine backwards.
#[test]
fn an_older_release_is_never_an_update() {
    for (current, listed) in [
        ("3.0.0", "v3.0.0-rc.11"),
        ("3.1.0", "v3.0.9"),
        ("3.0.0-rc.10", "v3.0.0-rc.9"),
        ("3.0.1-rc.1", "v3.0.0"),
    ] {
        assert!(
            !needs_update(current, listed).expect("both are release versions"),
            "{listed} is older than {current}, so it is not an update"
        );
    }
    assert!(
        needs_update("3.0.0-rc.9", "v3.0.0-rc.10").expect("both are release versions"),
        "candidates order numerically, not as text: rc.10 is after rc.9"
    );
}

/// A stable machine stays on stable releases, a candidate stays on candidates,
/// and the variable overrides either. An unknown value is refused rather than
/// silently treated as one of the two.
#[test]
fn the_channel_follows_the_running_binary_unless_the_operator_names_one() {
    let unset = MapEnv::new();
    assert_eq!(
        release_channel(&unset, "3.0.0").unwrap(),
        ReleaseChannel::Stable
    );
    assert_eq!(
        release_channel(&unset, "3.0.0-rc.11").unwrap(),
        ReleaseChannel::Prerelease
    );
    assert_eq!(
        release_channel(&unset, "dev").unwrap(),
        ReleaseChannel::Stable
    );
    let opted_in = MapEnv::new().with(RELEASE_CHANNEL_ENV, "prerelease");
    assert_eq!(
        release_channel(&opted_in, "3.0.0").unwrap(),
        ReleaseChannel::Prerelease
    );
    let opted_out = MapEnv::new().with(RELEASE_CHANNEL_ENV, "stable");
    assert_eq!(
        release_channel(&opted_out, "3.0.0-rc.11").unwrap(),
        ReleaseChannel::Stable
    );
    assert!(release_channel(&MapEnv::new().with(RELEASE_CHANNEL_ENV, "beta"), "3.0.0").is_err());
}

/// The stable channel never picks a candidate, and both channels pick the
/// highest version rather than whatever was published last.
#[test]
fn the_listing_resolves_the_highest_version_on_the_channel() {
    let listing = json!([
        {"tag_name": "v3.0.1-rc.1"},
        {"tag_name": "v3.0.0-rc.12"},
        {"tag_name": "v3.0.0"},
        {"tag_name": "v3.0.0-rc.11"},
        {"tag_name": "v0.5.0"},
    ]);
    assert_eq!(
        installable_tags(&listing, ReleaseChannel::Stable).next(),
        Some("v3.0.0")
    );
    assert_eq!(
        installable_tags(&listing, ReleaseChannel::Prerelease).next(),
        Some("v3.0.1-rc.1")
    );
    assert_eq!(
        installable_tags(
            &json!([{"tag_name": "v3.0.0-rc.9"}, {"tag_name": "v3.0.0-rc.10"}]),
            ReleaseChannel::Stable
        )
        .next(),
        None,
        "with no stable release the stable channel has nothing to install"
    );
}

#[test]
fn something_that_is_not_a_release_version_is_refused_rather_than_compared() {
    assert!(canonical_release_version("latest").is_err());
    assert!(canonical_release_version("3.1").is_err());
    assert!(canonical_release_version("3.1.0.1").is_err());
    assert!(canonical_release_version("3.1.0-").is_err());
    assert!(canonical_release_version("3.1.0-rc..2").is_err());
    assert_eq!(
        canonical_release_version("v3.1.0-rc.2+9f2c1ab").expect("a full tag is a release version"),
        "3.1.0-rc.2"
    );
}

/// A mirror pins the origin for the self-updater exactly as it already does for
/// the deploy paths. When the two resolved differently, `roost update` could not
/// be pointed at a mirror at all.
#[test]
fn a_configured_mirror_replaces_the_github_origin() {
    let mirrored = MapEnv::new().with(RELEASE_BASE_URL_ENV, "https://mirror.internal/roost");
    assert_eq!(
        release_base_url(&mirrored, "v3.0.0"),
        "https://mirror.internal/roost",
        "a mirror is the operator's choice of origin and outranks the tag's own directory"
    );
    assert_eq!(
        release_base_url(&MapEnv::new(), "v3.0.0"),
        format!("{RELEASE_DOWNLOAD_ORIGIN}/v3.0.0"),
        "with no mirror the RESOLVED TAG's directory is used, never `latest`"
    );
    assert_eq!(
        release_base_url(&MapEnv::new().with(RELEASE_BASE_URL_ENV, "   "), "v3.0.0"),
        format!("{RELEASE_DOWNLOAD_ORIGIN}/v3.0.0"),
        "an empty mirror variable is no mirror, not the empty origin"
    );
}

/// The self-updater must never install another series. This repository's
/// newest release today is a TypeScript `roost`, and a digest-verified download
/// of it would pass every check this module makes and replace a Rust binary
/// with a Bun one that answers none of this contract's commands.
#[test]
fn only_a_v3_tag_is_installable() {
    let listing = json!([
        {"tag_name": "v0.5.0", "draft": false},
        {"tag_name": "v3.0.0-rc.1", "draft": false},
        {"tag_name": "v3.0.0-rc.0", "draft": false},
    ]);
    assert_eq!(
        installable_tags(&listing, ReleaseChannel::Prerelease).next(),
        Some("v3.0.0-rc.1"),
        "a v2 tag above a v3 rc must not win: the rc is the highest INSTALLABLE release"
    );
    assert_eq!(
        installable_tags(
            &json!([{"tag_name": "v0.5.0"}, {"tag_name": "v0.4.0"}]),
            ReleaseChannel::Prerelease
        )
        .next(),
        None,
        "a listing with no v3 tag names nothing"
    );
    assert_eq!(
        installable_tags(
            &json!([
                {"tag_name": "v3.0.0-rc.2", "draft": true},
                {"tag_name": "v3.0.0-rc.1", "draft": false},
            ]),
            ReleaseChannel::Prerelease
        )
        .next(),
        Some("v3.0.0-rc.1"),
        "a draft has no assets anybody can fetch, so the next published tag is the answer"
    );
    assert_eq!(
        installable_tags(&json!({"message": "Not Found"}), ReleaseChannel::Prerelease).next(),
        None,
        "a body that is not a listing names nothing"
    );
}

/// A side-branch tag can outrank every real release: semver sorts an
/// alphanumeric pre-release identifier above a numeric one. The selector walks
/// the list past it, so the ranker's head is pinned here.
#[test]
fn version_order_puts_a_side_branch_tag_above_every_real_release() {
    let listing = json!([
        {"tag_name": "v3.0.0-rc.96"},
        {"tag_name": "v3.0.0-rc.win.4"},
        {"tag_name": "v3.0.0-rc.95"},
    ]);
    assert_eq!(
        installable_tags(&listing, ReleaseChannel::Prerelease).collect::<Vec<_>>(),
        ["v3.0.0-rc.win.4", "v3.0.0-rc.96", "v3.0.0-rc.95"],
        "highest version first; the v3 branch check is what disqualifies the head"
    );
}

/// Which compare answers may be installed. `identical` and `behind` are a tag
/// cut from `v3`; `ahead` and `diverged` carry work `v3` does not contain;
/// anything else is unreadable, which installs the candidate.
#[test]
fn only_a_tag_on_the_release_branch_is_installable() {
    for status in ["identical", "behind"] {
        assert_eq!(
            compare_status_allows(status),
            Some(true),
            "{status} is a tag cut from v3"
        );
    }
    for status in ["ahead", "diverged"] {
        assert_eq!(
            compare_status_allows(status),
            Some(false),
            "{status} is a commit v3 does not contain"
        );
    }
    for status in ["", "unknown", "Behind"] {
        assert_eq!(
            compare_status_allows(status),
            None,
            "{status:?} is not an answer this command reads"
        );
    }
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

#[test]
fn omp_bundle_contract_starts_with_the_3_0_5_release_line() {
    use roost_cli::update::version::ReleaseVersion;
    for (tag, bundled) in [
        ("v3.0.0-rc.39", false),
        ("v3.0.4", false),
        ("v3.0.5-rc.1", true),
        ("v3.0.5", true),
        ("v3.1.0", true),
    ] {
        assert_eq!(
            ReleaseVersion::parse(tag).unwrap().bundles_omp(),
            bundled,
            "{tag}"
        );
    }
}

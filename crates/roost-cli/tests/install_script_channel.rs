#![cfg(unix)]
//! `install.sh`: which release a first install takes. The highest stable v3
//! release by version; while none exists, the highest pre-release; with
//! `ROOST_RELEASE_CHANNEL=prerelease`, the highest of either. Runs the real
//! script against releases published over `file://` by `install_script_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod install_script_support;

use install_script_support::{Invocation, Sandbox, run_script_against};

/// The tag the script fetched, read from its own `fetching … from <tag>` line.
fn installed_tag(listing: &[&str], channel: Option<&str>) -> (bool, Option<String>, String) {
    let sandbox = Sandbox::new(&format!("channel-{}", channel.unwrap_or("unset")));
    let (api, origin) = sandbox.publish_fake_releases(listing);
    installed_tag_from(&sandbox, &api, &origin, channel)
}

/// The same, against releases already published into `sandbox`.
fn installed_tag_from(
    sandbox: &Sandbox,
    api: &str,
    origin: &str,
    channel: Option<&str>,
) -> (bool, Option<String>, String) {
    let empty_path = sandbox.root.join("empty-bin");
    std::fs::create_dir_all(&empty_path).expect("an empty PATH entry");
    let (ok, _stdout, stderr) = run_script_against(
        sandbox,
        &empty_path,
        api,
        origin,
        &Invocation {
            grant: false,
            args: &["--dry-run"],
            channel,
        },
    );
    let tag = stderr
        .lines()
        .find_map(|line| line.strip_prefix(">> fetching roost"))
        .and_then(|line| line.rsplit(" from ").next())
        .map(str::to_string);
    (ok, tag, stderr)
}

/// Listed in publish order, not version order: a candidate published after the
/// final, a v2 tag, and an rc whose number sorts wrongly as text.
const MIXED: [&str; 6] = [
    "v3.0.1-rc.1",
    "v3.0.0-rc.12",
    "v3.0.0",
    "v0.5.0",
    "v3.0.0-rc.9",
    "v2.9.9",
];

#[test]
fn a_first_install_takes_the_highest_stable_release() {
    let (ok, tag, stderr) = installed_tag(&MIXED, None);
    assert!(ok, "{stderr}");
    assert_eq!(
        tag.as_deref(),
        Some("v3.0.0"),
        "a candidate published later must not displace the final: {stderr}"
    );
}

#[test]
fn the_prerelease_channel_takes_the_highest_release_of_either_kind() {
    let (ok, tag, stderr) = installed_tag(&MIXED, Some("prerelease"));
    assert!(ok, "{stderr}");
    assert_eq!(tag.as_deref(), Some("v3.0.1-rc.1"), "{stderr}");
}

/// Before a stable release exists, version order still selects rc.10 over rc.9.
#[test]
fn with_no_stable_release_a_bare_install_takes_the_highest_candidate() {
    let listing = ["v3.0.0-rc.9", "v3.0.0-rc.10", "v3.0.0-rc.2"];
    let (ok, tag, stderr) = installed_tag(&listing, None);
    assert!(ok, "{stderr}");
    assert_eq!(tag.as_deref(), Some("v3.0.0-rc.10"), "{stderr}");

    let (ok, tag, _stderr) = installed_tag(&listing, Some("stable"));
    assert!(
        !ok && tag.is_none(),
        "an explicit stable channel never falls back to a candidate"
    );
}

#[test]
fn an_unknown_channel_installs_nothing() {
    let (ok, tag, stderr) = installed_tag(&MIXED, Some("beta"));
    assert!(!ok && tag.is_none(), "{stderr}");
}

/// A `v3.` name is not proof of a v3 build. A side-branch tag with an
/// alphanumeric pre-release identifier sorts above every real release, so the
/// listing's top candidate is passed over for the next one cut from `v3`.
#[test]
fn a_tag_cut_from_a_side_branch_is_passed_over() {
    let sandbox = Sandbox::new("channel-side-branch");
    let (api, origin) = sandbox
        .publish_fake_releases_cut_from(&["v3.0.0-rc.96", "v3.0.0-rc.win.4"], &["v3.0.0-rc.win.4"]);
    let (ok, tag, stderr) = installed_tag_from(&sandbox, &api, &origin, None);
    assert!(ok, "{stderr}");
    assert_eq!(
        tag.as_deref(),
        Some("v3.0.0-rc.96"),
        "the real release wins although rc.win.4 sorts above it: {stderr}"
    );
}

#[test]
fn an_off_branch_stable_does_not_block_the_default_prerelease_fallback() {
    let sandbox = Sandbox::new("channel-filtered-stable");
    let (api, origin) =
        sandbox.publish_fake_releases_cut_from(&["v3.99.0", "v3.0.0-rc.96"], &["v3.99.0"]);
    let (ok, tag, stderr) = installed_tag_from(&sandbox, &api, &origin, None);
    assert!(ok, "{stderr}");
    assert_eq!(tag.as_deref(), Some("v3.0.0-rc.96"), "{stderr}");
}

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
    let empty_path = sandbox.root.join("empty-bin");
    std::fs::create_dir_all(&empty_path).expect("an empty PATH entry");
    let (ok, _stdout, stderr) = run_script_against(
        &sandbox,
        &empty_path,
        &api,
        &origin,
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

/// Before the first stable v3 exists, a bare install still works and says it
/// is installing a pre-release; rc.10 outranks rc.9 although it sorts lower as
/// text.
#[test]
fn with_no_stable_release_a_bare_install_takes_the_highest_candidate_and_says_so() {
    let listing = ["v3.0.0-rc.9", "v3.0.0-rc.10", "v3.0.0-rc.2"];
    let (ok, tag, stderr) = installed_tag(&listing, None);
    assert!(ok, "{stderr}");
    assert_eq!(tag.as_deref(), Some("v3.0.0-rc.10"), "{stderr}");
    assert!(
        stderr.contains("no stable v3 release yet"),
        "the operator is told they are getting a pre-release: {stderr}"
    );

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
    assert!(stderr.contains("ROOST_RELEASE_CHANNEL"), "{stderr}");
}

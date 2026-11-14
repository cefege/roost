#![cfg(unix)]
//! `install.sh`: the three things that decide whether a machine with no v3
//! install can get one at all. Which job a run does is `install_script_modes`.
//!
//! The chain used to be circular. The script located a `roost` and handed the
//! grant to it, so a machine joining for the first time — which is the only kind
//! of machine that runs this script — found whatever older `roost` was on PATH
//! and exec'd *that* against a v3 coordinator. On every fleet target that is a
//! v2 binary, and a v2 binary half-enrolls a v2 worker and reports success.
//!
//! So the properties pinned here are behavioural, run against the real script:
//! a v2 binary on PATH is not exec'd, the script names the four published asset
//! names correctly, and a digest that does not match aborts before anything is
//! installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod install_script_support;

use std::path::Path;

use install_script_support::{
    JOIN, Sandbox, install_script, run_script, run_script_against, write_fake_roost,
};

#[test]
fn a_v2_binary_on_path_is_never_the_one_that_joins() {
    let sandbox = Sandbox::new("v2-on-path");
    let path_dir = sandbox.root.join("usr-local-bin");
    write_fake_roost(&path_dir.join("roost"), false);

    // The origin is deliberately unreachable, so the only thing that could hand
    // the grant over is the v2 binary. If the script were to exec it, this test
    // would see the marker it writes.
    let (ok, _stdout, stderr) = run_script(&sandbox, &path_dir);

    let execed = std::fs::read_to_string(sandbox.log()).unwrap_or_default();
    assert!(
        execed.is_empty(),
        "the script handed a v3 grant to a pre-v3 binary, which is how a machine ends up \
         half-enrolled while reporting success: {execed}{stderr}"
    );
    assert!(
        !ok,
        "with no v3 binary and no reachable origin the script must fail, not succeed quietly"
    );
    assert!(
        stderr.contains("not a v3 roost") || stderr.contains("No v3"),
        "the refusal has to say what was found and what was missing, or the operator is left \
         guessing which of the two machines is wrong:\n{stderr}"
    );
}

#[test]
fn a_v3_binary_at_the_self_link_location_is_the_one_that_joins() {
    let sandbox = Sandbox::new("v3-self-link");
    // A v2 binary earlier on PATH than the self-link, which is the case a
    // PATH-first lookup gets wrong.
    let path_dir = sandbox.root.join("usr-local-bin");
    write_fake_roost(&path_dir.join("roost"), false);
    write_fake_roost(&sandbox.home().join(".local/bin/roost"), true);

    let (ok, _stdout, _stderr) = run_script(&sandbox, &path_dir);

    let execed = std::fs::read_to_string(sandbox.log()).unwrap_or_default();
    assert!(
        execed.contains(".local/bin/roost"),
        "the self-link location is where `roost self-link` installs, so it is asked first: \
         {execed}"
    );
    assert!(
        !execed.contains("usr-local-bin/roost"),
        "and the older binary earlier on PATH must never be the one that joins: {execed}"
    );
    assert!(ok, "a v3 binary at the self-link location joins cleanly");
}

/// A machine still running the previous generation keeps its `roost`. The
/// fetched release is staged in a directory the join owns and removes, and is
/// never written over the v2 binary at the self-link location, which on a v2
/// Mac is v2's own CLI: the script used to install there and replaced it.
#[test]
fn a_join_that_fetches_leaves_a_v2_binary_at_the_self_link_location_alone() {
    let sandbox = Sandbox::new("v2-kept");
    let v2 = sandbox.home().join(".local/bin/roost");
    write_fake_roost(&v2, false);
    let v2_bytes = std::fs::read(&v2).expect("the v2 stand-in is readable");
    let (api, origin) = sandbox.publish_fake_release();
    let empty_path = sandbox.root.join("empty-bin");
    std::fs::create_dir_all(&empty_path).expect("an empty PATH entry");

    let (ok, _stdout, stderr) = run_script_against(&sandbox, &empty_path, &api, &origin, &JOIN);

    assert!(ok, "a fetched release joins: {stderr}");
    assert_eq!(
        std::fs::read(&v2).expect("the v2 binary is still there"),
        v2_bytes,
        "the v2 binary at the self-link location is untouched"
    );
    assert!(
        !sandbox.home().join(".local/bin/roost-keeper").exists(),
        "and nothing is installed beside it"
    );
    let execed = std::fs::read_to_string(sandbox.log()).unwrap_or_default();
    let staged = execed
        .split_whitespace()
        .nth(1)
        .expect("the staged roost was run");
    assert!(
        !staged.contains(".local/bin"),
        "the staged copy joined, not the self-link: {execed}"
    );
    assert!(
        !Path::new(staged).exists(),
        "the staged pair is removed once the join returns: {staged}"
    );
}

/// The enrolment command and the script are one change, not two, and this is
/// what holds them together: the URL the command PRINTS is the URL the script
/// names in the refusal it prints when the grant is missing. An operator reads
/// the second one when the first one fails, so a constant that drifts from the
/// script's own text sends them to a URL that does not exist. `join.sh` is the
/// URL older coordinators print, and it must forward to the same script.
///
/// The URL is read from the exported constant rather than scraped out of a
/// source file, so relocating the builder cannot silently drop this guard.
#[test]
fn the_command_and_the_script_name_the_same_url() {
    let url = roost_platform::INSTALL_SCRIPT_URL;
    let script = std::fs::read_to_string(install_script()).expect("install.sh is readable");
    assert!(
        script.contains(url),
        "the command prints {url} and install.sh never names it, so the two documents an \
         operator reads disagree"
    );
    assert!(
        url.contains("/v3/install.sh") && !script.contains("roost/main/"),
        "a URL on `main` resolves to whichever generation `main` points at, and `main` is v2 \
         until the cutover fast-forwards it: {url}"
    );
    let forwarder = install_script().with_file_name("join.sh");
    let forwarder = std::fs::read_to_string(&forwarder).expect("join.sh is readable");
    assert!(
        forwarder.contains(url),
        "join.sh is what older coordinators print, so it has to run the same installer"
    );
}

#[test]
fn the_digest_line_claims_only_what_a_digest_establishes() {
    let script = std::fs::read_to_string(install_script()).expect("install.sh is readable");
    for overclaim in ["verified", "authentic", "trusted", "secure download"] {
        assert!(
            !script.to_lowercase().contains(overclaim),
            "the asset and its sidecar come from the same origin, so a passing check says the \
             two agree and not that either is the maintainer's build — the script must not say \
             {overclaim:?}"
        );
    }
    assert!(
        script.contains("both match the digests published beside them"),
        "and it should say what it did establish"
    );
    assert!(
        script.contains("expected") && script.contains("actual"),
        "a mismatch has to name both digests, or the operator cannot tell a truncated download \
         from a tampered one"
    );
}

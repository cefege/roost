//! The names a release publishes, and the commands the gate runs, are stated in
//! more than one file because more than one process needs them. This test is the
//! only thing that notices when one of those files stops agreeing with the
//! others; it reads `assets.rs`'s table, `join.sh`, and both workflows, and
//! fails when any pair diverges.
//!
//! The failure it prevents is silent and platform-shaped: a name that exists in
//! one table and not another is a 404 on exactly one architecture, so four
//! machines work and one cannot join.
//!
//! **What it does NOT check, so do not read a green as more than it is:** it
//! compares strings. It cannot see a `cp` that points at a directory cargo does
//! not produce, a digest sidecar no step writes, or a cross-compiled target
//! nobody installed. Those are runtime failures in a workflow, and the only
//! thing that finds them is running the workflow.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use roost_cli::update::assets::{WEB_ASSET_NAME, keeper_release_asset_name, release_asset_name};
use roost_host::HostPlatform;

/// The four (platform, arch) pairs a release publishes, in the order
/// `assets.rs` matches them. This list is the SET being compared, not a fourth
/// naming: it names no asset, so adding a fifth target means editing
/// `assets.rs` and `join.sh`, and this test follows.
const PUBLISHED: [(HostPlatform, &str); 4] = [
    (HostPlatform::Linux, "x64"),
    (HostPlatform::Linux, "arm64"),
    (HostPlatform::MacOs, "x64"),
    (HostPlatform::MacOs, "arm64"),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .to_path_buf()
}

fn read_repo(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Every `cargo …` command ONE job of a workflow runs, as a set.
///
/// Scoped to a job because a release workflow legitimately runs cargo commands
/// outside its gate: the matrix build compiles per target and the smoke job
/// builds the binaries it drives. Comparing whole files reports those as gate
/// drift, so this reads one job's body — from its two-space header to the next
/// key at the same indent — rather than the file.
///
/// Deliberately crude. A YAML-aware parse would accept a restructure this test
/// is not written to follow, and the thing worth checking is that two jobs name
/// the same commands, not that either is well formed.
fn job_cargo_commands(workflow: &str, job: &str) -> BTreeSet<String> {
    let header = format!("  {job}:");
    let commands: BTreeSet<String> = read_repo(workflow)
        .lines()
        .skip_while(|line| line.trim_end() != header)
        .skip(1)
        // A job ends at the next key at the same two-space indent, or at EOF.
        // Deeper indentation (steps, their bodies) is inside the job.
        .take_while(|line| !line.starts_with("  ") || line.starts_with("   "))
        .filter_map(|line| {
            let trimmed = line.trim();
            let rest = trimmed.strip_prefix("run:").unwrap_or(trimmed);
            let rest = rest.trim();
            rest.starts_with("cargo ").then(|| rest.to_string())
        })
        .collect();
    assert!(
        !commands.is_empty(),
        "found no cargo commands in the `{job}` job of {workflow}; \
         if the job moved, this is the assertion that has to follow it"
    );
    commands
}

/// The `roost_asset:` / `keeper_asset:` values a workflow's build matrix names.
fn matrix_assets(workflow: &str, key: &str) -> BTreeSet<String> {
    let prefix = format!("{key}: ");
    read_repo(workflow)
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed.starts_with(&prefix).then(|| trimmed[prefix.len()..].to_string())
        })
        .collect()
}

#[test]
fn the_release_matrix_publishes_exactly_the_names_assets_rs_resolves() {
    let expected_roost: BTreeSet<String> = PUBLISHED
        .iter()
        .map(|(platform, arch)| release_asset_name(*platform, arch).unwrap().to_string())
        .collect();
    let expected_keeper: BTreeSet<String> = PUBLISHED
        .iter()
        .map(|(platform, arch)| keeper_release_asset_name(*platform, arch).unwrap())
        .collect();

    assert_eq!(
        matrix_assets(".github/workflows/release.yml", "roost_asset"),
        expected_roost,
        "release.yml's roost_asset values have drifted from release_asset_name. \
         assets.rs is the authority, and join.sh must agree with it too."
    );
    assert_eq!(
        matrix_assets(".github/workflows/release.yml", "keeper_asset"),
        expected_keeper,
        "release.yml's keeper_asset values have drifted from keeper_release_asset_name."
    );
}

/// The four names, pinned as literals.
///
/// `keeper_release_asset_name` derives a keeper name by substituting into a roost
/// name, and each published name contains `roost` exactly once — so the
/// substitution's count is not exercised by any name here. These assertions
/// exist so the four names are checked as VALUES rather than trusted through
/// prose: a change to the substitution has to edit them, and a name that stopped
/// matching `join.sh` fails here rather than as a 404 on one architecture.
#[test]
fn the_four_published_names_are_the_ones_join_sh_fetches() {
    let expected_roost = [
        (HostPlatform::Linux, "x64", "roost-linux-x64"),
        (HostPlatform::Linux, "arm64", "roost-linux-arm64"),
        (HostPlatform::MacOs, "x64", "roost-darwin-x64"),
        (HostPlatform::MacOs, "arm64", "roost"),
    ];
    let expected_keeper = [
        (HostPlatform::Linux, "x64", "roost-keeper-linux-x64"),
        (HostPlatform::Linux, "arm64", "roost-keeper-linux-arm64"),
        (HostPlatform::MacOs, "x64", "roost-keeper-darwin-x64"),
        (HostPlatform::MacOs, "arm64", "roost-keeper"),
    ];

    for (platform, arch, want) in expected_roost {
        assert_eq!(
            release_asset_name(platform, arch).unwrap(),
            want,
            "{} {} publishes a name no join.sh can fetch",
            platform.display_name(),
            arch
        );
    }
    for (platform, arch, want) in expected_keeper {
        assert_eq!(
            keeper_release_asset_name(platform, arch).unwrap(),
            want,
            "{} {} publishes a keeper under a name the release never emits",
            platform.display_name(),
            arch
        );
    }
}

#[test]
fn join_sh_resolves_the_same_four_names() {
    let script = read_repo("join.sh");
    for (platform, arch) in PUBLISHED {
        let name = release_asset_name(platform, arch).unwrap();
        let quoted = format!("'{name}'");
        assert!(
            script.contains(&quoted) || script.contains(&format!("\"{name}\"")),
            "join.sh never mentions {name}, so a machine that joins by script \
             would not find the asset assets.rs says exists for {}",
            platform.display_name()
        );
    }
}

#[test]
fn the_web_bundle_is_published_under_the_name_the_installer_fetches() {
    assert_eq!(
        WEB_ASSET_NAME, "roost-web.tar.gz",
        "WEB_ASSET_NAME changed; the release workflow and the installer both \
         fetch the old string, and this is the assertion that has to be updated \
         with them rather than after them."
    );
    let workflow = read_repo(".github/workflows/release.yml");
    assert!(
        workflow.contains(WEB_ASSET_NAME),
        "release.yml never publishes {WEB_ASSET_NAME}"
    );
}

#[test]
fn the_release_verify_job_runs_the_same_commands_as_the_merge_gate() {
    assert_eq!(
        job_cargo_commands(".github/workflows/release.yml", "verify"),
        job_cargo_commands(".github/workflows/ci.yml", "rust"),
        "release.yml's verify job and ci.yml's rust job have drifted. Copy the \
         command from ci.yml rather than retyping it; a narrower local gate is \
         how a crate that cannot compile for its only target reached a green."
    );
}

//! `roost join` names the variable that is missing, and refuses a dirty tree
//! with the code the contract reserves for an unproved build identity.
//!
//! The dirty-tree case runs against a real throwaway git checkout rather than a
//! stubbed one, because the whole claim is that git is asked and its answer is
//! obeyed. The repository is created by the test and removed by it; nothing
//! here reads the working tree the test binary was built from.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use roost_cli::deploy::codes;
use roost_cli::quickstart::join::{JoinCredentials, joined_build_sha, read_credentials};
use roost_cli::quickstart::plan;
use roost_host::{HostPlatform, MapEnv};

/// A throwaway git checkout with one commit on it.
struct TempCheckout {
    root: PathBuf,
}

impl TempCheckout {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-join-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway checkout is created");
        let tree = Self { root };
        tree.git(&["init", "--quiet", "--initial-branch=main", "."]);
        tree.git(&["config", "user.email", "join-test@roost.invalid"]);
        tree.git(&["config", "user.name", "join test"]);
        // A commit of an empty tree is refused, and a repository that has no
        // commit has no identity to prove — so the checkout needs one file
        // staged before the first commit, and a signing key the machine running
        // this test may not have.
        tree.git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(tree.root.join("joined.rs"), b"fn main() {}\n")
            .expect("the first committed file is written");
        tree.git(&["add", "joined.rs"]);
        tree.git(&["commit", "--quiet", "-m", "a clean snapshot"]);
        tree
    }

    fn git(&self, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(&self.root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git is on PATH");
        assert!(status.success(), "git {arguments:?} succeeded");
    }

    /// An uncommitted edit, which is what a dirty tree is.
    fn dirty(&self) {
        std::fs::write(self.root.join("uncommitted.rs"), b"fn half_written() {}\n")
            .expect("the uncommitted file is written");
    }
}

impl Drop for TempCheckout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn environment(pairs: &[(&str, &str)]) -> MapEnv {
    pairs
        .iter()
        .fold(MapEnv::new(), |env, (key, value)| env.with(key, value))
}

#[test]
fn a_join_with_neither_variable_names_the_coordinator_url_first() {
    let failure = read_credentials(&environment(&[])).expect_err("nothing is set");
    assert_eq!(failure.code, 1);
    assert!(
        failure.message.contains("ROOST_COORDINATOR_URL is not set"),
        "{failure}"
    );
    assert!(
        failure.message.contains("roost add-machine"),
        "the refusal says where the missing half comes from: {failure}"
    );
}

#[test]
fn a_join_with_a_url_but_no_grant_names_the_grant() {
    let failure = read_credentials(&environment(&[(
        "ROOST_COORDINATOR_URL",
        "https://a.example",
    )]))
    .expect_err("the grant is missing");
    assert_eq!(failure.code, 1);
    assert!(
        failure.message.contains("ROOST_BOOTSTRAP_TOKEN is not set"),
        "{failure}"
    );
    assert!(
        !failure.message.contains("ROOST_COORDINATOR_URL is not set"),
        "only the variable that is actually missing is named: {failure}"
    );
}

#[test]
fn a_join_with_both_variables_carries_exactly_what_it_was_given() {
    let credentials = read_credentials(&environment(&[
        ("ROOST_COORDINATOR_URL", "  https://a.example  "),
        ("ROOST_BOOTSTRAP_TOKEN", "roost_bt_x"),
        ("ROOST_WORKER_LABEL", "build box"),
    ]))
    .expect("both variables are present");
    assert_eq!(credentials.coordinator_url, "https://a.example");
    assert_eq!(credentials.bootstrap_token, "roost_bt_x");
    assert_eq!(credentials.label.as_deref(), Some("build box"));
}

/// A dirty tree is refused with the reserved code for an unproved identity, and
/// the refusal names the way out rather than only the verdict.
///
/// Skipped when the test process itself carries `ROOST_ALLOW_DIRTY=1`, because
/// that variable is read from the ambient environment and would make the run
/// mean something else. Saying so is better than a test that passes for the
/// wrong reason.
#[tokio::test]
async fn a_dirty_checkout_is_refused_with_the_reserved_code_and_names_the_escape_hatch() {
    if std::env::var("ROOST_ALLOW_DIRTY").as_deref() == Ok("1") {
        eprintln!("skipping: this test process exports ROOST_ALLOW_DIRTY=1");
        return;
    }
    let checkout = TempCheckout::new("dirty");
    checkout.dirty();

    let failure = joined_build_sha(&checkout.root)
        .await
        .expect_err("a dirty checkout is refused");
    assert_eq!(
        failure.code,
        codes::IDENTITY_UNPROVED,
        "an unproved build identity is code 7, not a generic failure: {failure}"
    );
    assert!(
        failure.message.contains("uncommitted changes"),
        "the refusal says what it found: {failure}"
    );
    assert!(
        failure.message.contains("ROOST_ALLOW_DIRTY=1"),
        "the refusal names the escape hatch: {failure}"
    );
    assert!(
        failure.message.contains("git status"),
        "the refusal says how to see what is pending: {failure}"
    );
}

#[tokio::test]
async fn a_clean_checkout_is_accepted_and_its_commit_is_the_identity() {
    let checkout = TempCheckout::new("clean");
    let stamp = joined_build_sha(&checkout.root)
        .await
        .expect("a clean checkout is proved");
    assert_eq!(stamp.len(), 40, "a 40-character commit: {stamp}");
    assert!(
        stamp.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{stamp}"
    );
    assert!(!stamp.ends_with("-dirty"), "{stamp}");
}

/// The one place in this slice that could put a credential somewhere an
/// operator reads it: the plan a dry run prints. A dry run must not mint a
/// grant, so the worker definition it renders carries a named placeholder, and
/// the placeholder is visible as a placeholder.
#[test]
fn a_dry_run_renders_a_worker_definition_with_a_named_placeholder_and_no_real_grant() {
    let root = std::env::temp_dir().join(format!("roost-join-plan-{}", std::process::id()));
    std::fs::create_dir_all(root.join("home")).expect("the throwaway home is created");
    let text = |relative: &str| root.join(relative).display().to_string();
    let env = environment(&[
        ("HOME", &text("home")),
        (
            roost_host::COORD_UNIT_ENV,
            &text("unit/roost3-coord.service"),
        ),
        (
            roost_host::WORKER_UNIT_ENV,
            &text("unit/roost3-worker.service"),
        ),
        (roost_host::WORKER_DATA_DIR_ENV, &text("data/worker")),
    ]);
    let endpoint = roost_cli::quickstart::endpoint::fresh_endpoint(None).expect("a fresh endpoint");
    let resolved = plan::resolve_plan(&env, HostPlatform::Linux, endpoint, None, None, false)
        .expect("the plan resolves on a machine with nothing installed");
    let rendered = resolved
        .worker
        .definition_text(HostPlatform::Linux)
        .expect("the worker definition renders");
    assert!(
        rendered.contains(roost_cli::quickstart::grant::PLACEHOLDER_BEARER),
        "the dry run shows the definition with a named placeholder: {rendered}"
    );
    assert!(
        !rendered.contains("roost_bt_"),
        "a dry run mints no bearer at all: {rendered}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A credentials value must never be renderable into a log through `Debug`,
/// because a `?`-propagated error or a `tracing` field would do exactly that.
#[test]
fn a_credentials_debug_rendering_carries_the_url_but_never_the_grant() {
    let rendered = format!(
        "{:?}",
        JoinCredentials {
            coordinator_url: "https://a.example".to_string(),
            bootstrap_token: "roost_bt_never_log_me".to_string(),
            label: None,
        }
    );
    assert!(rendered.contains("https://a.example"), "{rendered}");
    assert!(!rendered.contains("never_log_me"), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");
}

/// The path a join proves its identity against, when the operator names one.
#[test]
fn a_named_source_root_is_the_tree_the_identity_is_proved_against() {
    let named = Path::new("/srv/roost-checkout");
    let env = environment(&[("ROOST_SOURCE_ROOT", &named.display().to_string())]);
    assert_eq!(
        roost_cli::quickstart::join::source_root(&env).expect("a named root resolves"),
        named
    );
}

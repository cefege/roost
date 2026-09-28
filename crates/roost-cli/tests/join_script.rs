//! `join.sh`: the three things that decide whether a machine with no v3
//! install can join at all.
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

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The script this crate ships and the enrolment command points at. It is one
/// file outside `src/`, so its path is derived rather than restated.
fn join_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../join.sh")
        .canonicalize()
        .expect("join.sh exists at the repository root")
}

/// A stand-in for a `roost` binary. `v3` advertises `import-v2` in its help, a
/// subcommand the previous generation has never had; `v2` does not, which is
/// exactly how `join.sh` tells them apart.
fn write_fake_roost(path: &Path, is_v3: bool) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory is created");
    let help = if is_v3 {
        "  import-v2  Carry a v2 coordinator's account, devices and keys"
    } else {
        "  status      Health readout"
    };
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n\
             # A stand-in that records being exec'd, so a test can tell a refusal\n\
             # from a hand-off.\n\
             if [ \"$1\" = \"--help\" ]; then echo \"{help}\"; exit 0; fi\n\
             echo \"EXECED $0 $*\" >> \"$ROOST_TEST_JOIN_LOG\"\n\
             exit 0\n"
        ),
    )
    .expect("the fake roost is written");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake roost is executable");
}

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-join-script-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("the home is created");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// The `PATH` a login-less ssh shell would carry, holding `path_dir`.
    fn path(&self, path_dir: &Path) -> String {
        let mut entries = vec![path_dir.display().to_string()];
        for essential in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            entries.push(essential.to_string());
        }
        entries.join(":")
    }

    fn log(&self) -> PathBuf {
        self.root.join("execed.log")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Run the script with the environment a pasted one-liner gives it, and return
/// what it wrote, both streams and the exit status.
fn run_script(sandbox: &Sandbox, path_dir: &Path) -> (bool, String, String) {
    let mut command = std::process::Command::new("bash");
    command
        .arg(join_script())
        .env("HOME", sandbox.home())
        .env("PATH", sandbox.path(path_dir))
        .env("ROOST_COORDINATOR_URL", "https://coordinator.example")
        .env("ROOST_BOOTSTRAP_TOKEN", "roost_bt_test")
        .env("ROOST_TEST_JOIN_LOG", sandbox.log())
        .env(
            "ROOST_RELEASE_BASE_URL",
            sandbox.root.join("no-such-origin").display().to_string(),
        )
        .env(
            "ROOST_RELEASE_API_URL",
            sandbox.root.join("no-such-api").display().to_string(),
        )
        .stdin(std::process::Stdio::null());
    let output = command.output().expect("bash runs the script");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

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

#[test]
fn the_script_publishes_the_v3_branch_and_not_the_one_that_still_holds_v2() {
    let script = std::fs::read_to_string(join_script()).expect("join.sh is readable");
    assert!(
        !script.contains("roost/main/join.sh"),
        "a URL on `main` hands the operator whichever generation `main` points at, and `main` \
         is v2 until the cutover fast-forwards it"
    );
    assert!(
        script.contains("roost/v3/join.sh"),
        "the usage text a missing grant prints has to name the same ref the enrolment command \
         prints, or the two documents the operator reads disagree"
    );
}

/// The enrolment command and the script are one change, not two, and this is
/// what holds them together: the URL the command PRINTS is the URL the script
/// names in the refusal it prints when the grant is missing. An operator reads
/// the second one when the first one fails, so a constant that drifts from the
/// script's own text sends them to a URL that does not exist.
///
/// The command itself is not called here: a grant can only be minted against a
/// coordinator's database, and this property is about the two documents
/// agreeing, not about the grant. So the constant is read where it is declared.
#[test]
fn the_command_and_the_script_name_the_same_url() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/quickstart/add_machine.rs"),
    )
    .expect("add_machine.rs is readable");
    let url = source
        .lines()
        .find(|line| line.trim_start().starts_with("const JOIN_SCRIPT_URL"))
        .and_then(|line| line.split('"').nth(1))
        .unwrap_or_else(|| panic!("JOIN_SCRIPT_URL is declared as a literal: {source}"));
    let script = std::fs::read_to_string(join_script()).expect("join.sh is readable");
    assert!(
        script.contains(url),
        "the command prints {url} and join.sh never names it, so the two documents an \
         operator reads disagree"
    );
    assert!(
        url.contains("/v3/join.sh"),
        "a URL on `main` resolves to whichever generation `main` points at, and `main` is v2 \
         until the cutover fast-forwards it: {url}"
    );
}

#[test]
fn the_digest_line_claims_only_what_a_digest_establishes() {
    let script = std::fs::read_to_string(join_script()).expect("join.sh is readable");
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

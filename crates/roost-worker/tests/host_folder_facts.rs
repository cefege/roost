//! What this worker can say about a machine and about a session's folder.
//!
//! Two kinds of case. The parsers are pure and are driven with the exact text
//! the tools print, because that is where the interesting failures live: a
//! loopback bind that leaks through, a `pid=` that belongs to somebody else, a
//! rollup that says "passing" while one check failed. The readers are driven
//! with fixture programs, so "gh is missing" and "gh is not authenticated" are
//! the same test rather than a property of whatever this machine has installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use roost_host::{HostPlatform, supported_host_platform};
use roost_protocol::wire::session::{PullRequestChecks, PullRequestState};
use roost_worker::host::git_branch::{GitReader, github_owner_repo};
use roost_worker::host::identity::{IdentitySources, collect_host_identity, os_release_value};
use roost_worker::host::ports::{parse_reachable_listen_ports, parse_ss_listen_ports, ports_eq};
use roost_worker::host::pr_status::{PrReader, RollupEntry, pull_request_state, rollup_checks};
use roost_worker::host::samples::{sample_disk, sample_linux_memory};
use roost_worker::host::sampling::{FactsSink, HostWatchers};
use scratch::Scratch;

fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}

/// Write an executable script into the scratch and return its path.
fn script(root: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).expect("the fixture makes its own directory");
    std::fs::write(&path, body).expect("the fixture writes its program");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the fixture program is executable");
    path.display().to_string()
}

/// A `gh` that answers with exactly this text and exits zero.
fn gh_answering(root: &Path, name: &str, body: &str) -> PrReader {
    PrReader::new(script(
        root,
        name,
        &format!("#!/bin/sh\ncat <<'ROOST_GH_EOF'\n{body}\nROOST_GH_EOF\n"),
    ))
}

/// A loopback bind is not reachable from another device, so its chip is a dead
/// link: a localhost-only vite, a language server and a debugger all bind here
/// and none of them answers on the worker's tailnet address.
#[test]
fn a_loopback_bind_is_not_a_reachable_port() {
    let lsof = "\
COMMAND   PID USER   FD   TYPE DEVICE SIZE/OFF NODE NAME
node    1234 alice   21u  IPv4  12345      0t0  TCP *:5174 (LISTEN)
node    1234 alice   22u  IPv4  12345      0t0  TCP 127.0.0.1:5175 (LISTEN)
node    1234 alice   23u  IPv6  12345      0t0  TCP [::1]:5176 (LISTEN)
node    1234 alice   24u  IPv6  12345      0t0  TCP [::]:5177 (LISTEN)
node    1234 alice   25u  IPv4  12345      0t0  TCP 100.101.102.103:8080 (LISTEN)
node    1234 alice   26u  IPv4  12345      0t0  TCP *:5174 (LISTEN)
";
    assert_eq!(parse_reachable_listen_ports(lsof), vec![5174, 5177, 8080]);
}

/// `ss` has no pid selector, so the tree filter happens in the parser. A row
/// belonging to another user's process is not this session's port, and without
/// the filter a session's chips would become the host's port list.
#[test]
fn an_ss_row_for_another_process_is_not_this_session_s_port() {
    let rows = "\
LISTEN 0      511          0.0.0.0:5173       0.0.0.0:*    users:((\"bun\",pid=4321,fd=20))
LISTEN 0      511          0.0.0.0:9999       0.0.0.0:*    users:((\"node\",pid=9999,fd=20))
LISTEN 0      511             [::1]:7000          [::]:*    users:((\"bun\",pid=4321,fd=21))
LISTEN 0      511          0.0.0.0:5174       0.0.0.0:*    users:((\"bun\",pid=4321,fd=22))
";
    let mine: BTreeSet<u32> = BTreeSet::from([4321]);
    assert_eq!(parse_ss_listen_ports(rows, &mine), vec![5173, 5174]);
    let none: BTreeSet<u32> = BTreeSet::from([1]);
    assert!(parse_ss_listen_ports(rows, &none).is_empty());
    assert!(ports_eq(&[], &[]));
    assert!(!ports_eq(&[5173], &[5173, 5174]));
}

/// EVERY `gh` failure is no badge, never an error: absent, unauthenticated,
/// empty, and unparseable are four different incidents and one answer. A folder
/// row that is briefly unbadged is a far smaller incident than a spawn that
/// throws because GitHub is down.
#[test]
fn gh_failing_for_every_reason_resolves_to_no_pull_request() {
    let scratch = Scratch::new("gh-failures");
    let folder = scratch.path("repo");
    std::fs::create_dir_all(&folder).expect("the fixture makes its folder");
    let folder = folder.to_str().unwrap();

    let failing = PrReader::new(script(scratch.root(), "gh", "#!/bin/sh\nexit 1\n"));
    let silent = gh_answering(scratch.root(), "gh-silent", "");
    let garbage = gh_answering(scratch.root(), "gh-garbage", "not json at all");
    let absent = PrReader::new(scratch.path("no-such-gh").display().to_string());

    for reader in [&failing, &silent, &garbage, &absent] {
        assert_eq!(
            reader.status(folder, "main"),
            None,
            "a failing gh must resolve to no badge, not to an error"
        );
    }
}

/// The happy path, and the proof that the states are the PROTOCOL's rather than
/// a second spelling of the same four words: a rollup that says "passing" while
/// one required check failed is a lie somebody merges on.
#[test]
fn a_pull_request_row_becomes_the_protocols_own_states() {
    let scratch = Scratch::new("gh-row");
    let folder = scratch.path("repo");
    std::fs::create_dir_all(&folder).expect("the fixture makes its folder");
    let row = r#"[{"number":412,"state":"OPEN","isDraft":false,"url":"https://github.com/o/r/pull/412","statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"},{"status":"COMPLETED","conclusion":"FAILURE"}]}]"#;
    let reader = gh_answering(scratch.root(), "gh", row);

    let status = reader
        .status(folder.to_str().unwrap(), "feature")
        .expect("a well-formed row is a badge");

    assert_eq!(status.number, 412);
    assert_eq!(status.state, PullRequestState::Open);
    assert_eq!(status.checks, PullRequestChecks::Failing);
    assert_eq!(status.url, "https://github.com/o/r/pull/412");
}

/// The four rollup shapes, and the rule that a failure outranks a pending
/// sibling. An empty rollup is `none`, never `passing`: no checks is not a
/// passing run.
#[test]
fn a_rollup_is_classified_by_its_worst_member() {
    let entry = |status: &str, conclusion: &str, state: Option<&str>| RollupEntry {
        status: status.to_string(),
        conclusion: conclusion.to_string(),
        state: state.map(str::to_string),
    };
    assert_eq!(rollup_checks(&[]), PullRequestChecks::None);
    assert_eq!(
        rollup_checks(&[entry("COMPLETED", "SUCCESS", None)]),
        PullRequestChecks::Passing
    );
    assert_eq!(
        rollup_checks(&[entry("COMPLETED", "", Some("SUCCESS"))]),
        PullRequestChecks::Passing
    );
    assert_eq!(
        rollup_checks(&[entry("QUEUED", "", None)]),
        PullRequestChecks::Pending
    );
    assert_eq!(
        rollup_checks(&[entry("", "", Some("PENDING"))]),
        PullRequestChecks::Pending
    );
    assert_eq!(
        rollup_checks(&[
            entry("IN_PROGRESS", "", None),
            entry("COMPLETED", "FAILURE", None)
        ]),
        PullRequestChecks::Failing
    );
    assert_eq!(
        rollup_checks(&[entry("", "", Some("ERROR"))]),
        PullRequestChecks::Failing
    );
    assert_eq!(
        pull_request_state("MERGED", false),
        PullRequestState::Merged
    );
    assert_eq!(
        pull_request_state("closed", false),
        PullRequestState::Closed
    );
    assert_eq!(pull_request_state("OPEN", true), PullRequestState::Draft);
    assert_eq!(
        pull_request_state("SOMETHING_NEW", false),
        PullRequestState::Open
    );
}

/// A detached HEAD reads as `@<short-sha>`: the row then shows something stable
/// and true instead of an empty subtitle. A folder that is not a repository, and
/// a `git` that is not installed, are both "nothing to show".
#[test]
fn a_detached_head_reads_as_a_short_sha_and_a_non_repo_reads_as_nothing() {
    let scratch = Scratch::new("git-reader");
    let folder = scratch.path("repo");
    std::fs::create_dir_all(&folder).expect("the fixture makes its folder");
    let folder = folder.to_str().unwrap();
    let detached = script(
        scratch.root(),
        "git-detached",
        "#!/bin/sh\ncase \"$2\" in\n\
         *--abbrev-ref*) echo HEAD ;;\n\
         *--short*) echo 0a1b2c3 ;;\n\
         *) echo .git/HEAD ;;\n\
         esac\n",
    );
    let on_branch = script(
        scratch.root(),
        "git-branch",
        "#!/bin/sh\ncase \"$2\" in\n\
         *--abbrev-ref*) echo feature/thing ;;\n\
         *--git-path*) echo .git/HEAD ;;\n\
         *) echo git@github.com:owner/repo.git ;;\n\
         esac\n",
    );
    let failing = script(scratch.root(), "git-missing", "#!/bin/sh\nexit 128\n");

    assert_eq!(
        GitReader::new(&detached).branch(folder).as_deref(),
        Some("@0a1b2c3")
    );
    let reader = GitReader::new(&on_branch);
    assert_eq!(reader.branch(folder).as_deref(), Some("feature/thing"));
    assert_eq!(reader.remote(folder).as_deref(), Some("owner/repo"));
    assert_eq!(reader.head_path(folder), Some(folder_path(scratch.root())));
    assert_eq!(GitReader::new(&failing).branch(folder), None);
}

/// The remote is read from either spelling, and a remote that is not GitHub is
/// `None` rather than a guess — the value is only ever used to ask GitHub about
/// a pull request.
#[test]
fn a_github_remote_is_read_from_either_spelling() {
    assert_eq!(
        github_owner_repo("git@github.com:owner/repo.git").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(
        github_owner_repo("https://github.com/owner/repo").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(github_owner_repo("https://gitlab.com/owner/repo"), None);
    assert_eq!(github_owner_repo(""), None);
}

fn folder_path(scratch: &Path) -> PathBuf {
    scratch.join("repo").join(".git/HEAD")
}

/// The watchers are keyed by session id and owned here, and stopping one JOINS
/// its thread: a watcher that is merely flagged is a thread that still holds
/// what it read until it next wakes, and a closed session that keeps reading is
/// a poll nobody turned off. The second `stop` reports there was nothing left,
/// which is what "released" means from out here.
#[test]
fn a_watcher_stopped_for_a_closed_session_releases_its_thread() {
    let platform = platform();
    let watchers = HostWatchers::new();
    let emitted = Arc::new(AtomicUsize::new(0));
    let sink_counter = Arc::clone(&emitted);
    let sink: FactsSink = Arc::new(move |_session, _facts| {
        sink_counter.fetch_add(1, Ordering::SeqCst);
    });
    let folder = std::env::temp_dir();

    assert!(watchers.watch(
        "session-a",
        &folder.display().to_string(),
        None,
        platform,
        sink.clone()
    ));
    assert!(
        !watchers.watch(
            "session-a",
            &folder.display().to_string(),
            None,
            platform,
            sink
        ),
        "a second watcher for one session would emit a stale duplicate reading"
    );
    assert!(watchers.is_watching("session-a"));
    assert_eq!(watchers.watched(), 1);

    assert!(watchers.stop("session-a"));
    assert!(!watchers.is_watching("session-a"));
    assert_eq!(watchers.watched(), 0);
    assert_eq!(
        watchers.live_watchers(),
        0,
        "the watcher thread outlived its session"
    );
    assert!(
        !watchers.stop("session-a"),
        "there was nothing left to release"
    );
}

/// A machine that cannot answer `/proc` or `vm_stat` must report ZEROS, not
/// refuse: a sampler that returned an error would mean no heartbeat at all, and
/// the machine would vanish from the fleet view instead of looking idle.
#[test]
#[cfg(target_os = "linux")]
fn a_sampler_reports_this_hosts_memory_and_disk_rather_than_refusing() {
    let (used, total) = sample_linux_memory();
    assert!(total > 0, "this host reported no memory at all");
    assert!(
        used <= total,
        "more memory used than exists: {used} of {total}"
    );
    let (disk_used, disk_total) = sample_disk();
    assert!(disk_total > 0, "the root filesystem reported no size");
    assert!(disk_used <= disk_total);
}

/// The identity is normalised by the PROTOCOL's own normaliser, so a value that
/// passes here is a value the coordinator will accept, and an identity of three
/// nulls is no identity at all.
#[test]
fn a_host_identity_is_collected_from_this_hosts_sources_or_is_none() {
    let sources = FixtureSources::new(
        "PRETTY_NAME=\"Fedora Linux 42 (Workstation Edition)\"\nNAME=Fedora\n".to_string(),
    );
    let linux = collect_host_identity(HostPlatform::Linux, &sources)
        .expect("a distribution is an identity");
    assert_eq!(
        linux.linux_distribution.as_deref(),
        Some("Fedora Linux 42 (Workstation Edition)")
    );
    assert!(linux.hardware_model.is_none());
    let apple = collect_host_identity(
        HostPlatform::MacOs,
        &FixtureSources {
            distribution: String::new(),
            chip: "Apple M3 Max".to_string(),
            model: "Mac16,6".to_string(),
        },
    )
    .expect("a chip is an identity");
    assert_eq!(apple.chip.as_deref(), Some("Apple M3 Max"));
    assert_eq!(apple.hardware_model.as_deref(), Some("Mac16,6"));
    // An Intel string in the brand field is not a chip name, and a value that
    // is nothing but whitespace is nothing at all.
    assert!(
        collect_host_identity(
            HostPlatform::MacOs,
            &FixtureSources {
                distribution: String::new(),
                chip: "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz".to_string(),
                model: "   ".to_string(),
            },
        )
        .is_none(),
        "an unclassifiable identity was reported as one"
    );
    assert!(collect_host_identity(HostPlatform::Windows, &sources).is_none());
}

/// `os-release` quoting and escaping, which is where a distribution name picks
/// up a stray quote if it is read naively.
#[test]
fn an_os_release_value_is_unquoted_and_unescaped() {
    let source = "NAME=\"Fedora Linux\"\nPRETTY_NAME=\"Fedora Linux 42 (Workstation Edition)\"\nQUOTED=\"a\\\\b\\\"c\\$d\"\n";
    assert_eq!(
        os_release_value(source, "PRETTY_NAME"),
        Some("Fedora Linux 42 (Workstation Edition)")
    );
    assert_eq!(os_release_value(source, "QUOTED"), Some("a\\b\"c$d"));
    assert_eq!(os_release_value(source, "MISSING"), None);
    // A key that is a PREFIX of another key must not match it.
    assert_eq!(os_release_value(source, "NAME"), Some("Fedora Linux"));
}

/// Sources that answer from a fixture rather than from this machine, which is
/// the only way an `os-release` and a `sysctl` are readable on a host that has
/// neither.
struct FixtureSources {
    distribution: String,
    chip: String,
    model: String,
}

impl FixtureSources {
    fn new(distribution: String) -> Self {
        Self {
            distribution,
            chip: String::new(),
            model: String::new(),
        }
    }
}

impl IdentitySources for FixtureSources {
    fn sysctl(&self, name: &str) -> Option<String> {
        match name {
            "machdep.cpu.brand_string" if !self.chip.is_empty() => Some(self.chip.clone()),
            "hw.model" if !self.model.trim().is_empty() => Some(self.model.clone()),
            _ => None,
        }
    }

    fn os_release(&self) -> Option<String> {
        (!self.distribution.is_empty()).then(|| self.distribution.clone())
    }
}

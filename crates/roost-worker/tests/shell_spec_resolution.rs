//! What a resolved launch contract guarantees, and what it must never carry.
//!
//! `is_keeper_control_key` is already tested as a predicate. What matters is
//! downstream of it, so the security case here opens a REAL PTY through a REAL
//! keeper and reads the environment out of the child: a strip that is missing,
//! applied to the wrong copy of the spec, or case-sensitive all fail there and
//! nowhere else. What the overlay and the worker's `ROOST_` namespace contribute
//! is `shell_spec_overlay`'s.
//!
//! The rest is the resolution contract: a folder that is materialised, a shell
//! that is resolved, terminal and locale variables that are set rather than
//! inherited, a `PATH` that can find `gh`, and a platform that is checked HERE
//! rather than four frames later inside a keeper.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

#[path = "keeper_pool_support/mod.rs"]
mod keeper_pool_support;

#[path = "shell_spec_support/mod.rs"]
mod shell_spec_support;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, channel, child_environment, opened, session};
use roost_host::HostPlatform;
use roost_worker::host::shell_bootstrap::ShellFlavour;
use roost_worker::host::shell_spec_resolver::PTY_PATH_PREFIX;
use roost_worker::host::tool_path::tool_path;
use roost_worker::session::sinks::ChannelBinding;
use scratch::Scratch;
use shell_spec_support::{platform, resolver, resolver_with};

/// The terminal and locale variables are SET, not inherited. A worker's own
/// `TERM` is `dumb` under a service manager and its `LANG` is often `C`; both
/// reaching the shell as the user's own answer is how a browser ends up
/// rendering a shell that cannot draw.
#[test]
fn a_resolved_spec_sets_the_terminal_and_locale_variables_explicitly() {
    let scratch = Scratch::new("spec-terminal");
    let resolver = resolver(scratch.root(), "/bin/sh");
    let folder = scratch.path("folder");
    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-terminal")
        .expect("a /bin/sh on this host resolves");

    assert_eq!(spec.env_value("TERM"), Some("xterm-256color"));
    assert_eq!(spec.env_value("COLORTERM"), Some("truecolor"));
    // The default is per-platform, and the service's `C` above is neither.
    let locale = match platform() {
        HostPlatform::MacOs => "en_US.UTF-8",
        _ => "C.UTF-8",
    };
    assert_eq!(spec.env_value("LANG"), Some(locale));
    assert_eq!(spec.env_value("LC_ALL"), Some(locale));
    assert_eq!(spec.platform, platform());
    assert_eq!(spec.version, roost_worker::shell_spec::SHELL_SPEC_VERSION);
}

/// The refusal happens HERE. A platform that is not this host is refused before
/// the folder is created, so nothing on this machine was touched on the way to
/// the answer — a spawn that failed deep in a platform path is a failure nobody
/// sees.
#[test]
fn a_platform_that_is_not_this_host_is_refused_before_anything_is_created() {
    let scratch = Scratch::new("spec-platform");
    let other = match platform() {
        HostPlatform::Linux => HostPlatform::MacOs,
        _ => HostPlatform::Linux,
    };
    let resolver = resolver_with(
        scratch.root(),
        "/bin/sh",
        BTreeMap::new(),
        platform(),
        other,
    );
    let folder = scratch.path("never-created");

    let refusal = resolver
        .resolve(folder.to_str().unwrap(), "session-platform")
        .expect_err("a session for another platform must be refused");

    assert!(
        refusal.contains(other.as_str()) && refusal.contains(platform().as_str()),
        "the refusal must name both platforms: {refusal}"
    );
    assert!(
        !folder.exists(),
        "the folder was created before the platform was checked"
    );
}

/// A folder the user named in a browser and has never visited is the common
/// case, so resolution creates it rather than handing the keeper a path that is
/// not there.
#[test]
fn a_session_folder_is_created_by_the_resolution() {
    let scratch = Scratch::new("spec-folder");
    let resolver = resolver(scratch.root(), "/bin/sh");
    let folder = scratch.path("deep/nested/folder");
    assert!(!folder.exists());

    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-folder")
        .expect("a /bin/sh on this host resolves");

    assert!(folder.is_dir(), "the session folder was not materialised");
    assert_eq!(spec.cwd, folder.display().to_string());
}

/// A missing binary is a refusal with a reason, not an ENOENT at the keeper.
#[test]
fn a_shell_that_is_not_on_this_host_is_refused_with_a_reason() {
    let scratch = Scratch::new("spec-missing-shell");
    let resolver = resolver(scratch.root(), "/nonexistent/roost-shell-that-is-not-here");
    let folder = scratch.path("folder");

    let refusal = resolver
        .resolve(folder.to_str().unwrap(), "session-missing")
        .expect_err("a shell that does not exist must be refused");

    assert!(
        refusal.contains("roost-shell-that-is-not-here"),
        "the refusal must name what it could not find: {refusal}"
    );
}

/// The `gh`/`lsof` fix, asserted on the value a PTY is launched with: the
/// package managers' directories are in FRONT of the inherited `PATH`, because
/// a worker's service unit does not carry them and a bare `gh` then ENOENTs
/// with the PR badge silently never resolving.
#[test]
fn a_pty_path_starts_with_the_package_manager_directories() {
    let scratch = Scratch::new("spec-path");
    let resolver = resolver(scratch.root(), "/bin/sh");
    let folder = scratch.path("folder");

    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-path")
        .expect("a /bin/sh on this host resolves");
    let path = spec
        .env_value("PATH")
        .expect("a PTY is launched with a PATH");

    assert!(
        path.starts_with(PTY_PATH_PREFIX),
        "the package-manager directories are not in front: {path}"
    );
    assert!(
        path.contains("/usr/local/bin:/usr/bin:/bin"),
        "the inherited PATH was dropped rather than kept behind: {path}"
    );
    // NOT `.starts_with(PTY_PATH_PREFIX)`. The two prefixes DIFFER on purpose
    // and `tool_path`'s own header says why: the worker's list carries
    // `/usr/sbin`, which holds root's tools, and a PTY is the user's shell
    // rather than this daemon. Asserting they match would demand the removal
    // of a documented difference, and the property worth pinning is the one
    // the message names: the tool PATH still resolves a tool, and it kept the
    // PTY PATH behind it rather than dropping it.
    let tool = tool_path(Some(path), platform());
    assert!(
        tool.starts_with("/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/usr/bin:/bin"),
        "the tool PATH lost the package-manager directories: {tool}"
    );
    assert!(
        tool.contains(path),
        "a tool PATH built from a PTY PATH must keep that PATH reachable, or a \\
         tool the user's shell could run is unreachable to the worker: {tool}"
    );
}

/// The shell is given somewhere to report its folder from, and `↑` recall
/// survives a restart. Both are files on disk, so both are asserted there.
#[test]
fn a_shell_is_launched_with_a_bootstrap_that_reports_its_folder() {
    let scratch = Scratch::new("spec-bootstrap");
    // A stand-in named `bash`, rather than this machine's bash: the resolver
    // decides from the FILE NAME, so a fixture shell is both the thing under
    // test and a host with no bash cannot fail it.
    let shell = scratch.path("bin/bash");
    std::fs::create_dir_all(shell.parent().unwrap()).expect("the fixture makes its own bin");
    std::fs::write(&shell, "#!/bin/sh\nexit 0\n").expect("the fixture writes its shell");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755))
        .expect("the fixture shell is executable");
    let resolver = resolver(scratch.root(), shell.to_str().unwrap());
    let folder = scratch.path("folder");

    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-bootstrap")
        .expect("an executable shell on this host resolves");

    assert_eq!(spec.executable, shell.display().to_string());
    assert_eq!(ShellFlavour::of(&spec.executable), ShellFlavour::Bash);
    let rcfile = spec.argv.last().expect("bash is launched with a --rcfile");
    assert_eq!(spec.argv.first().map(String::as_str), Some("--rcfile"));
    let rcfile = Path::new(rcfile);
    assert!(rcfile.is_file(), "the bootstrap rcfile was not written");
    let body = std::fs::read_to_string(rcfile).expect("the rcfile is readable");
    assert!(
        body.contains("osc7"),
        "the bootstrap does not report the folder"
    );
    let history = spec.env_value("HISTFILE").expect("a history file is named");
    assert!(
        history.starts_with(&scratch.path("home").display().to_string()),
        "the history file is not per home: {history}"
    );
    assert_eq!(spec.env_value("SAVEHIST"), Some("10000"));
}

/// The bash bootstrap, run by a real bash: every finished command reports its
/// status with OSC 133 `D` before the next prompt's `A`, `PS0` marks each
/// command's output with `C`, and the user's own `PROMPT_COMMAND` (a starship,
/// say) still sees the command's real `$?`. bash older than 4.4 (macOS's
/// bundled 3.2) has no PS0, so it gets no `C` mark and the assertion expects
/// none. Skipped on a host without bash.
#[test]
fn the_bash_bootstrap_marks_commands_and_keeps_the_users_exit_status() {
    let Ok(bash) = which_bash() else {
        return;
    };
    let version = std::process::Command::new(&bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"])
        .output()
        .expect("bash reports its version");
    let mut parts = String::from_utf8_lossy(&version.stdout)
        .split_whitespace()
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect::<Vec<_>>()
        .into_iter();
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    let has_ps0 = major > 4 || (major == 4 && minor >= 4);
    let scratch = Scratch::new("spec-bootstrap-run");
    let home = scratch.path("home");
    std::fs::create_dir_all(&home).expect("the fixture makes a home");
    std::fs::write(home.join(".bashrc"), "PROMPT_COMMAND='echo user-saw-$?'\n")
        .expect("the fixture writes the user's bashrc");
    let rcfile = roost_worker::host::shell_bootstrap::ensure_bootstrap(
        &scratch.path("bootstrap"),
        ShellFlavour::Bash,
    )
    .expect("the bootstrap is written");
    let mut child = std::process::Command::new(&bash)
        .args(["--noprofile", "--rcfile"])
        .arg(&rcfile)
        .arg("-i")
        .env("HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("bash starts");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("bash has a stdin");
        stdin
            .write_all(b"false\ntrue\n")
            .expect("the commands are written");
    }
    let output = child
        .wait_with_output()
        .expect("bash exits at end of input");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let failed = stdout
        .find("\x1b]133;D;1\x07")
        .expect("`false` reports status 1");
    let succeeded = stdout
        .find("\x1b]133;D;0\x07")
        .map(|first| {
            stdout[first + 1..]
                .find("\x1b]133;D;0\x07")
                .map_or(first, |next| first + 1 + next)
        })
        .expect("`true` reports status 0");
    assert!(failed < succeeded, "statuses out of order: {stdout:?}");
    assert!(
        stdout.contains("user-saw-1"),
        "the user's PROMPT_COMMAND lost the real exit status: {stdout:?}"
    );
    let expected_c = if has_ps0 { 2 } else { 0 };
    assert_eq!(
        stderr.matches("\x1b]133;C\x07").count(),
        expected_c,
        "PS0 (bash 4.4+) marks each command's output once; bash {major}.{minor}: {stderr:?}"
    );
}

fn which_bash() -> Result<std::path::PathBuf, ()> {
    ["/bin/bash", "/usr/bin/bash", "/opt/homebrew/bin/bash"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
        .ok_or(())
}

/// A shell script the user writes has to be able to tell which session it is
/// running inside, so the id travels in the PTY's own environment and not only
/// on the record.
#[test]
fn the_session_id_reaches_the_shell_as_its_own_variable() {
    let scratch = Scratch::new("spec-session-id");
    let resolver = resolver(scratch.root(), "/bin/sh");
    let folder = scratch.path("folder");

    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-identity")
        .expect("a /bin/sh on this host resolves");

    assert_eq!(spec.env_value("ROOST_SESSION_ID"), Some("session-identity"));
}

/// The capability property, END TO END. Every spelling a case-SENSITIVE check
/// would wave through, put into the service environment this worker inherited;
/// then a real PTY is opened with the resolved spec and its CHILD is asked what
/// it was handed. A worker that leaks a credential here hands every command a
/// user types the ability to speak to the keeper as this worker.
#[test]
fn a_keeper_control_credential_never_reaches_a_spawned_child() {
    let scratch = Scratch::new("spec-capability");
    let resolver = resolver(scratch.root(), "/bin/sh");
    let folder = scratch.path("folder");
    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-capability")
        .expect("a /bin/sh on this host resolves");

    let keeper = KeeperFixture::start();
    let pool = keeper.pool();
    let (binding, record) = session("resolver-capability");
    // The program is swapped for a probe that prints what it was handed; the
    // ENVIRONMENT is the resolver's output, byte for byte, which is the thing
    // under test.
    let mut probe = spec.clone();
    probe.argv = vec!["-c".to_string(), "env".to_string()];

    opened(
        pool.spawn(
            channel(1),
            &probe,
            80,
            24,
            Arc::new(binding) as Arc<dyn ChannelBinding>,
        ),
        "the keeper opens a real PTY with the resolved environment",
    );
    let seen = record.settled();
    assert_eq!(seen.error, None, "the channel failed instead of running");
    let environment = child_environment(&seen);

    // The child really did print an environment, so what follows is about THIS
    // environment and not about a channel that produced nothing at all.
    assert!(
        environment
            .iter()
            .any(|(key, value)| key == "ROOST_SESSION_ID" && value == "session-capability"),
        "the child's environment was not printed: {environment:?}"
    );
    assert!(
        environment
            .iter()
            .any(|(key, value)| key == "TERM" && value == "xterm-256color"),
        "the child did not inherit the resolved terminal: {environment:?}"
    );
    let leaked: Vec<&String> = environment
        .iter()
        .map(|(key, _)| key)
        .filter(|key| roost_worker::shell_spec::is_keeper_control_key(key))
        .collect();
    assert!(
        leaked.is_empty(),
        "a keeper control credential reached the child: {leaked:?}"
    );
    assert!(
        !environment
            .iter()
            .any(|(key, value)| key == "ROOST_KEEPER_ENDPOINT" && value == "/run/keeper.sock"),
        "the worker's own keeper endpoint reached the child"
    );
}

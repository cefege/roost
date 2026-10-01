//! `roost worker` — the subcommand a service manager, a container entrypoint
//! and the smoke stack all address — has to BOOT, not refuse.
//!
//! WHY THIS IS A PROCESS TEST AND NOT A CALL TEST. The defect it pins is not a
//! value; it is which entry point the subcommand takes. `roost_worker::serve`
//! builds and owns a tokio runtime for callers outside one, and refuses — by
//! name — when it finds itself already inside one. `roost_coord::serve` beside
//! it is `async` for exactly that reason. Calling the blocking form from an
//! `async fn` compiles, passes every unit test in the crate, and produces a
//! worker that starts, prints one line and exits. A supervisor sees a
//! crash-looping service; the Playwright stack sees "routable timed out after
//! 30000ms" on every single spec that needs a worker.
//!
//! So the property is observed the only way it can be: run the real binary,
//! give it a coordinator nothing is listening on, and read what it wrote before
//! the dial had any chance to succeed. Reaching the worker's own key handling
//! means the boot sequence ran, which is the thing the refusal prevented, and
//! the absence of the refusal is the thing the fix restored.
//!
//! BOTH STREAMS ARE READ, and that is not tidiness. The boot log goes to stdout
//! and the refusal goes to stderr, so a test that watches one of them sees a
//! healthy boot in both worlds and passes against the broken binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The `roost` binary cargo built as part of this test run.
fn roost_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_roost"))
}

struct TempDir {
    dir: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let unique = format!(
            "roost-worker-subcommand-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique.replace(['(', ')', ' '], ""));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Self { dir }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The refusal the runtime-owning entry point prints when it finds itself inside
/// a runtime. Named rather than reconstructed: it is the one string in the
/// worker that means "this process is wired wrong", and a paraphrase would let
/// a reworded message pass this gate.
const RUNTIME_REFUSAL: &str = "serve owns its runtime";

/// The boot line that proves the boot sequence ran, past the key and before the
/// dial.
const KEY_HANDLING: &str = "roost_worker::host::jwt";

/// Drain one pipe into `sink` on its own thread, so neither stream can fill its
/// buffer and wedge the child while the other is being read.
fn drain<R: Read + Send + 'static>(
    mut pipe: R,
    sink: Arc<Mutex<String>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        while let Ok(read) = pipe.read(&mut buffer) {
            if read == 0 {
                break;
            }
            sink.lock()
                .expect("the sink mutex is not poisoned")
                .push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    })
}

/// Stop the worker AND whatever it spawned, without leaving an orphan.
///
/// `roost worker` starts a `roost-keeper` child, and killing the worker alone
/// leaves that keeper holding a PTY, a socket and a pid file for the rest of
/// the machine's life. The cost is invisible in this test's own result — it is
/// one orphan per run — and enough of them saturate the box until the OTHER
/// suites' timing starts to move, which is how a keeper cadence regression was
/// nearly blamed on the wrong constant.
///
/// The children are found by pid rather than by process group because signalling
/// a group needs `libc::kill`, and this crate forbids `unsafe` and does not
/// depend on `libc`. `pkill -P` is the same statement without either.
fn stop_worker_and_its_children(child: &mut std::process::Child) {
    let _ = std::process::Command::new("pkill")
        .arg("-KILL")
        .arg("-P")
        .arg(child.id().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
}

/// Run `roost worker` against a coordinator that is not there, and return
/// everything it wrote.
///
/// The coordinator URL is a loopback port nothing is bound to, so a healthy
/// worker boots, dials, and keeps retrying — which is its normal life, not a
/// crash. A broken one exits at once. Either way the run ends when the deadline
/// passes, the child is killed, and both pipes are closed by the kill.
fn run_worker_until(timeout: Duration) -> String {
    let temp = TempDir::new();
    let mut child = Command::new(roost_binary())
        .arg("worker")
        .arg("--coordinator-url")
        .arg("http://127.0.0.1:1")
        .env("HOME", &temp.dir)
        .env("ROOST_WORKER_DATA_DIR", temp.dir.join("worker-data"))
        .env("ROOST_COORD_DATA_DIR", temp.dir.join("coord-data"))
        .env("ROOST_COORD_LOG_DIR", temp.dir.join("logs"))
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the roost binary starts");

    let sink = Arc::new(Mutex::new(String::new()));
    let readers = vec![
        drain(
            child.stdout.take().expect("stdout was piped"),
            Arc::clone(&sink),
        ),
        drain(
            child.stderr.take().expect("stderr was piped"),
            Arc::clone(&sink),
        ),
    ];

    // Wait for the boot line, then give the process a moment to say anything
    // else. The key line is NOT where this test stops: the broken worker prints
    // it on the way IN, because the boot reaches the key before it reaches the
    // call that refuses, so stopping there would read the proof of a healthy
    // boot and never the refusal that follows.
    let deadline = Instant::now() + timeout;
    let mut settle: Option<Instant> = None;
    while Instant::now() < deadline {
        let written = sink.lock().expect("the sink mutex is not poisoned").clone();
        if settle.is_none() && written.contains(KEY_HANDLING) {
            settle = Some(Instant::now() + Duration::from_millis(2_000));
        }
        if settle.is_some_and(|until| Instant::now() >= until) {
            break;
        }
        if child.try_wait().expect("the child is waitable").is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    stop_worker_and_its_children(&mut child);
    let _ = child.wait();
    for reader in readers {
        let _ = reader.join();
    }
    sink.lock().expect("the sink mutex is not poisoned").clone()
}

#[test]
fn the_worker_subcommand_boots_instead_of_refusing_to_share_a_runtime() {
    let written = run_worker_until(Duration::from_secs(30));

    assert!(
        !written.contains(RUNTIME_REFUSAL),
        "`roost worker` refused to start because it called the runtime-owning \
         entry point from inside the CLI's own runtime, so the worker never \
         served. It printed: {written:?}"
    );
    assert!(
        written.contains(KEY_HANDLING),
        "the boot never reached the worker's own key handling, so this binary \
         failed somewhere else entirely and this test would be reporting the \
         wrong defect. It printed: {written:?}"
    );
}

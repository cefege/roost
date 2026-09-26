// A support module that cannot say what it expected is not a support module.
// `expect` is denied outside `#[cfg(test)]`, and an integration-test module is
// its own crate, so the exemption is here.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Compiled once per test binary, and each binary drives a different subset of
// it, so an item unused by one is not dead code.
#![allow(dead_code)]

//! A REAL keeper under the pool: a real socket, a real PTY, a real child, and a
//! session binding that reads back what that child produced. Every assertion
//! these tests make about a spawn is therefore an assertion about the shipped
//! spawn path rather than about a predicate or a fake.
//!
//! What calls it: `keeper_pool_spawn.rs` (what a spawn guarantees) and
//! `keeper_pool_channels.rs` (what the channel table guarantees). Depends on
//! `roost_keeper`'s server, on `roost_worker`'s pool and shell spec — nothing
//! here is specific to one of them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use roost_keeper::server::{Endpoint, Server};
use roost_worker::keeper_pool::{KeeperPool, PoolError};
use roost_worker::runtime::keeper_boot::KeeperHandle;
use roost_worker::session::sinks::ChannelBinding;
use roost_worker::shell_spec::{SHELL_SPEC_VERSION, ShellSpec};

/// How long a test waits for the keeper or a child to do its part. Generous
/// because four other agents are building on the same eight cores.
pub const PATIENCE: Duration = Duration::from_secs(20);

/// What a session would have seen, if this were a real one.
#[derive(Debug, Default)]
pub struct Recording {
    state: Mutex<Seen>,
    finished: Condvar,
}

#[derive(Debug, Default)]
pub struct Seen {
    /// PTY bytes, exactly as the keeper delivered them.
    pub output: Vec<u8>,
    /// `Some(code)` once the child ended. The inner `Option` is the wire's: a
    /// child killed by a signal is not a child that exited zero.
    pub exit: Option<Option<i32>>,
    pub error: Option<String>,
}

impl ChannelBinding for Recording {
    fn on_output(&self, chunk: &[u8]) {
        self.lock().output.extend_from_slice(chunk);
    }

    fn on_exit(&self, exit_code: Option<i32>) {
        let mut seen = self.lock();
        seen.exit = Some(exit_code);
        self.finished.notify_all();
    }

    fn on_error(&self, reason: String) {
        let mut seen = self.lock();
        seen.error = Some(reason);
        self.finished.notify_all();
    }
}

impl Recording {
    fn lock(&self) -> MutexGuard<'_, Seen> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Wait for the channel to end, and report what it saw either way.
    pub fn settled(&self) -> Seen {
        let deadline = Instant::now() + PATIENCE;
        let mut seen = self.lock();
        while seen.exit.is_none() && seen.error.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "the child never ended: {seen:?}");
            let (guard, _) = self
                .finished
                .wait_timeout(seen, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            seen = guard;
        }
        seen
    }

    /// Wait until the child has printed `needle`, and return what it printed.
    ///
    /// The waiting release is for a child that is MEANT to keep running: a
    /// channel whose child has exited is already out of the announced set, so a
    /// test that wanted to see it announced would be asserting on a race.
    pub fn printed(&self, needle: &str) -> String {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let text = child_text(&self.lock());
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "the child never printed {needle:?}; it printed {text:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// A binding plus the name of its test, so a failure says which session it was.
///
/// `ChannelBinding` deliberately carries no `Debug` bound, and a test that fails
/// without saying which of eight concurrent channels it was wastes the run that
/// found it.
#[derive(Clone)]
pub struct Session {
    name: &'static str,
    record: Arc<Recording>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name)
    }
}

impl ChannelBinding for Session {
    fn on_output(&self, chunk: &[u8]) {
        self.record.on_output(chunk);
    }

    fn on_exit(&self, exit_code: Option<i32>) {
        self.record.on_exit(exit_code);
    }

    fn on_error(&self, reason: String) {
        self.record.on_error(reason);
    }
}

/// A named session a test can read back.
pub fn session(name: &'static str) -> (Session, Arc<Recording>) {
    let record = Arc::new(Recording::default());
    let binding = Session {
        name,
        record: Arc::clone(&record),
    };
    (binding, record)
}

/// A real keeper on a real socket, in a directory of its own.
pub struct KeeperFixture {
    socket: std::path::PathBuf,
    root: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl KeeperFixture {
    pub fn start() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!("roost-keeper-pool-{unique}"));
        std::fs::create_dir_all(&root).expect("the fixture can make its own directory");
        let socket = root.join("keeper.sock");
        let endpoint = Endpoint::new(&socket).expect("the endpoint is a socket path");
        let mut server = Server::bind(endpoint).expect("the fixture can bind a keeper socket");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match server.accept_one() {
                    Some(stream) => {
                        server.serve_one(stream);
                    }
                    None => return,
                }
            }
        });
        Self {
            socket,
            root,
            stop,
            thread: Some(thread),
        }
    }

    /// A pool driving this keeper, with its dispatch loop running.
    pub fn pool(&self) -> Arc<KeeperPool> {
        let client =
            roost_keeper::client::connect(&self.socket).expect("the fixture keeper answers");
        KeeperPool::new(KeeperHandle::new(client))
    }
}

impl Drop for KeeperFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The keeper thread is parked in accept; connecting is what wakes it,
        // and the throwaway connection then ends on EOF so the loop sees the stop.
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Unwrap a pool request, naming what was being attempted when it failed.
pub fn opened<T>(outcome: Result<T, PoolError>, what: &str) -> T {
    match outcome {
        Ok(value) => value,
        Err(err) => panic!("{what}: {err}"),
    }
}

/// The error a pool request produced, for a test that wants the refusal.
pub fn refused<T>(outcome: Result<T, PoolError>, what: &str) -> PoolError {
    match outcome {
        Ok(_) => panic!("{what}, but the request succeeded"),
        Err(err) => err,
    }
}

/// Wait for a condition the pool reaches on its own dispatch thread.
pub fn wait_until(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A spec running `/bin/sh` in a directory that exists.
///
/// It carries `PATH`, because the keeper clears the environment it did not
/// inherit (`roost_keeper::pty_channel` calls `env_clear`, which is v2 parity:
/// `Bun.spawn` treated `env` as a replace) and a resolved spec is what decides
/// what a login shell gets. A test child that could not find `sleep` would be a
/// fixture that never ran its command, not a pool defect.
pub fn sh_spec(args: &[&str], env: &[(&str, &str)]) -> ShellSpec {
    let mut entries: Vec<(String, String)> = env
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    if !entries.iter().any(|(key, _)| key == "PATH") {
        entries.push((
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        ));
    }
    ShellSpec {
        version: SHELL_SPEC_VERSION,
        platform: roost_host::HostPlatform::Linux,
        executable: "/bin/sh".to_string(),
        argv: args.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: std::env::temp_dir().display().to_string(),
        env: entries,
    }
}

/// The environment a child actually saw, keyed as it printed them.
pub fn child_environment(seen: &Seen) -> Vec<(String, String)> {
    String::from_utf8_lossy(&seen.output)
        .split('\n')
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (key, value) = line.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// The text a child printed, with the PTY's carriage returns removed.
pub fn child_text(seen: &Seen) -> String {
    String::from_utf8_lossy(&seen.output).replace('\r', "")
}

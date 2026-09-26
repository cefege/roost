//! The machinery one boot test needs and the other eight do not: a keeper on a
//! real socket that admits, a resolved boot configuration, a service definition
//! on disk, and a subscriber that records what the spend reported.
//!
//! Every part here is real. The keeper is a real Unix socket speaking the real
//! handshake; the definition is a real file a real rename rewrites; the events
//! are the ones `tracing` emitted. A fake that answered `Ok` where the code
//! under test asks "did the file change" would assert nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_host::{HostPlatform, MapEnv, supported_host_platform};
use roost_keeper::codec::{CodecError, FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::frames::{ChannelBinding, ListChannelsResp};
use roost_keeper::payloads::{
    KEEPER_PROTOCOL_VERSION, KeeperContractV1, KeeperFeature, KeeperHelloResponse,
    KeeperObservation,
};
use roost_worker::runtime::boot::{ENV_KEEPER_EXECUTABLE, WorkerBoot};
use roost_worker::runtime::serve_until;
use roost_worker::runtime::stop::{StopReason, StopRequests};

/// The authorisation this test is about.
pub const FORCE_LIVE_RETIRE_KEY: &str = "ROOST_KEEPER_FORCE_LIVE_RETIRE";

/// A variable beside it in the same definition, which the erase must not take.
pub const SURVIVOR_KEY: &str = "ROOST_COORDINATOR_URL";

/// This test only runs where v3 runs.
pub fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}

/// A worker configuration resolved entirely inside `root`.
///
/// The keeper executable is THIS TEST BINARY, and that is load-bearing: the
/// worker hashes whatever path it was given and compares that digest against
/// what the keeper at the endpoint reports, so a fixture keeper is only admitted
/// when it reports the digest of a file that exists and is readable.
pub fn boot(root: &Path, platform: HostPlatform) -> WorkerBoot {
    let executable = std::env::current_exe().expect("a running test has a binary");
    let env = MapEnv::new()
        .with(
            "ROOST_WORKER_KEY_PATH",
            root.join("worker.key").to_str().unwrap(),
        )
        .with("ROOST_WORKER_LOG_DIR", root.join("logs").to_str().unwrap())
        .with(
            "ROOST_KEEPER_SOCKET",
            root.join("keeper.sock").to_str().unwrap(),
        )
        .with(ENV_KEEPER_EXECUTABLE, executable.to_str().unwrap());
    WorkerBoot::resolve(&env, platform).expect("a worker configuration resolves in a scratch")
}

/// Run one activation to its end, having asked it to stop first.
///
/// The stop is requested BEFORE the run, not during it: the property under test
/// is spent between keeper admission and the link, and a link loop that had to
/// be interrupted would make the test depend on dial timing.
pub async fn serve_once(boot: WorkerBoot) -> anyhow::Result<()> {
    let (requests, _signal) = StopRequests::channel();
    requests.request(StopReason::Signal("SIGTERM"));
    serve_until(boot, requests).await
}

/// A real keeper socket that authenticates, speaks this protocol, and reports
/// the bindings it holds — none.
pub struct FakeKeeper {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeKeeper {
    /// Bind the socket the boot configuration names and answer on it.
    pub async fn start(boot: &WorkerBoot, platform: HostPlatform) -> Self {
        // NOT `keeper_binary_digest`. Asking the function under test what to
        // report makes the comparison `x == x`: it passed while the worker
        // hashed with `DefaultHasher` and a real keeper with SHA-256, and
        // would have passed just as happily with the comparison inverted. The
        // fixture computes the digest the way a REAL keeper does, so the test
        // fails when the worker's side is wrong.
        let digest = roost_keeper::keeper::implementation_digest_of(&boot.keeper_executable)
            .expect("a running test binary is readable");
        let socket = boot.keeper_socket.clone();
        let listener = std::os::unix::net::UnixListener::bind(&socket)
            .expect("the fixture can bind the keeper socket the boot names");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let contract = contract(&digest, platform);
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { return };
                let contract = contract.clone();
                std::thread::spawn(move || serve(stream, contract));
            }
        });
        Self {
            stop,
            socket,
            thread: Some(thread),
        }
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accept loop is parked in `accept`; connecting is what wakes it.
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
    }
}

impl Drop for FakeKeeper {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The contract this fixture keeper reports.
///
/// Both feature lists are sorted because the wire validator requires it, and a
/// fixture that reported them in enum order would be refused for a reason that
/// has nothing to do with the property under test.
fn contract(digest: &str, platform: HostPlatform) -> KeeperContractV1 {
    let names = |features: &[KeeperFeature]| {
        let mut names: Vec<String> = features.iter().map(|f| f.wire_name().to_string()).collect();
        names.sort();
        names
    };
    KeeperContractV1 {
        protocol_version: KEEPER_PROTOCOL_VERSION,
        supported_features: names(&KeeperFeature::SUPPORTED),
        required_features: names(&KeeperFeature::REQUIRED),
        implementation_digest: Some(digest.to_string()),
        platform: platform.as_str().to_string(),
        arch: std::env::consts::ARCH.to_string(),
        build_sha: "retire-authorization-fixture".to_string(),
    }
}

/// Answer one connection until the peer goes away.
fn serve(mut stream: std::os::unix::net::UnixStream, contract: KeeperContractV1) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        for event in decoder.push(&buffer[..read]) {
            let StreamEvent::Frame { frame_type, .. } = event else {
                return;
            };
            let reply = match frame_type {
                Some(MuxFrameType::Hello) => hello(&contract),
                Some(MuxFrameType::ListChannels) => MuxFrame::json(
                    MuxFrameType::ListChannelsResp,
                    0,
                    &ListChannelsResp {
                        channels: Vec::<ChannelBinding>::new(),
                    },
                ),
                _ => continue,
            };
            if let Ok(reply) = reply {
                if stream.write_all(&reply.encode()).is_err() {
                    return;
                }
            }
        }
    }
}

fn hello(contract: &KeeperContractV1) -> Result<MuxFrame, CodecError> {
    MuxFrame::json(
        MuxFrameType::HelloResp,
        0,
        &KeeperHelloResponse {
            contract: contract.clone(),
            observation: KeeperObservation {
                contract: contract.clone(),
                live_channel_count: 0,
            },
            // Every feature this build requires, which is what the client's own
            // hello refuses a keeper for lacking.
            features: KeeperFeature::REQUIRED.to_vec(),
        },
    )
}

/// The worker's service definition, on disk, in the shape the platform's
/// service manager uses.
pub struct Definition {
    path: PathBuf,
}

impl Definition {
    /// Write a definition into `root` and point the PROCESS environment at it,
    /// which is the only environment `runtime::serve_until` spends through.
    pub fn write(root: &Path, platform: HostPlatform, with_authorisation: bool) -> Self {
        let (path, body) = match platform {
            HostPlatform::MacOs => (
                root.join("com.roost.worker-v3.plist"),
                plist(with_authorisation),
            ),
            _ => (root.join("roost3-worker.service"), unit(with_authorisation)),
        };
        std::fs::write(&path, body).expect("the fixture can write its service definition");
        // SAFETY: the tests that read this variable are serialised by the
        // caller's mutex, and nothing else in this binary reads it.
        unsafe {
            std::env::set_var(
                match platform {
                    HostPlatform::MacOs => "ROOST_WORKER_PLIST_ENV",
                    _ => "ROOST_WORKER_UNIT_ENV",
                },
                &path,
            );
        }
        Self { path }
    }

    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).expect("the definition is still there")
    }
}

fn unit(with_authorisation: bool) -> String {
    let authorisation = if with_authorisation {
        format!("Environment=\"{FORCE_LIVE_RETIRE_KEY}=1\"\n")
    } else {
        String::new()
    };
    format!(
        "[Unit]\nDescription=roost worker\n\n[Service]\n{authorisation}\
         Environment=\"{SURVIVOR_KEY}=http://127.0.0.1:4113\"\nExecStart=/usr/bin/roost-worker\n"
    )
}

fn plist(with_authorisation: bool) -> String {
    let authorisation = if with_authorisation {
        format!("\t<key>{FORCE_LIVE_RETIRE_KEY}</key>\n\t<string>1</string>\n")
    } else {
        String::new()
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
         \t<key>EnvironmentVariables</key>\n\t<dict>\n{authorisation}\t\
         <key>{SURVIVOR_KEY}</key>\n\t<string>http://127.0.0.1:4113</string>\n\t</dict>\n\
         </dict></plist>\n"
    )
}

/// Record `removed_from_service_definition` from every event emitted on the
/// current thread, and hand back the reader AND the guard that keeps the
/// subscriber installed. Dropping the guard uninstalls it mid-test.
///
/// Thread-local on purpose: `install_observability` inside `serve_until` accepts
/// a subscriber that is already installed, and a global one would also swallow
/// the events of whatever else this test binary is doing in parallel.
pub fn spend_events() -> (
    impl Fn() -> Vec<Option<bool>>,
    tracing::subscriber::DefaultGuard,
) {
    let recorded: Arc<Mutex<Vec<Option<bool>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    let guard = tracing::subscriber::set_default(SpendCapture(sink));
    let read = move || recorded.lock().expect("the capture lock").clone();
    (read, guard)
}

/// A subscriber that keeps one `Option<bool>` per spend event.
struct SpendCapture(Arc<Mutex<Vec<Option<bool>>>>);

impl tracing::Subscriber for SpendCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = RemovedField(None);
        event.record(&mut visitor);
        if let Some(removed) = visitor.0 {
            self.0.lock().expect("the capture lock").push(Some(removed));
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Reads the one field this test is about, and ignores every other one.
struct RemovedField(Option<bool>);

impl tracing::field::Visit for RemovedField {
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "removed_from_service_definition" {
            self.0 = Some(value);
        }
    }
}

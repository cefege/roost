//! The machinery one boot test needs and the other eight do not: a keeper on a
//! real socket that admits, a resolved boot configuration, and a service
//! definition on disk in the shape the platform's service manager uses.
//!
//! The definition is spent by a real activation, and that activation runs in a
//! child process: `runtime::serve_until` spends through `ProcessEnv`, the
//! process environment is the only channel to it, and mutating it is `unsafe`
//! in edition 2024, which this workspace forbids outright.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use roost_host::{HostPlatform, supported_host_platform};
use roost_keeper::codec::{CodecError, FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::frames::{ChannelBinding, ListChannelsResp};
use roost_keeper::payloads::{
    KEEPER_PROTOCOL_VERSION, KeeperContractV1, KeeperFeature, KeeperHelloResponse,
    KeeperObservation,
};
use roost_worker::runtime::boot::WorkerBoot;
use roost_worker::runtime::serve_until;
use roost_worker::runtime::stop::{StopReason, StopRequests};


// `../` because a `#[path]` inside a `mod.rs` resolves against THIS file's
// directory, and the shared builder sits one level up beside every other
// test-root support module. The session-spawn support module reaches its
// sibling the same way.
#[path = "../boot_env_support/mod.rs"]
mod boot_env;
mod child;

pub use child::{position_of, serve_in_child, spends};

/// The authorisation this test is about.
pub const FORCE_LIVE_RETIRE_KEY: &str = "ROOST_KEEPER_FORCE_LIVE_RETIRE";

/// A variable beside it in the same definition, which the erase must not take.
pub const SURVIVOR_KEY: &str = "ROOST_COORDINATOR_URL";

/// This test only runs where v3 runs.
pub fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}

/// A worker configuration resolved entirely inside `root`, from the shared
/// builder.
///
/// The keeper executable is THIS TEST BINARY, and that is load-bearing: the
/// worker hashes whatever path it was given and compares that digest against
/// what the keeper at the endpoint reports, so a fixture keeper is only
/// admitted when it reports the digest of a file that exists and is readable.
/// The builder sets it, and a fixture naming a keeper which is not there gets
/// refused at admission, which reads as a keeper defect and is not one.
pub fn boot(root: &Path, platform: HostPlatform) -> WorkerBoot {
    boot_env::resolve_boot_env(root, platform)
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
    /// Write a definition into `root` and keep its path, which is what the
    /// activation is pointed at when the child is spawned.
    pub fn write(root: &Path, host: HostPlatform, with_authorisation: bool) -> Self {
        let (path, body) = match host {
            HostPlatform::MacOs => (
                root.join("com.roost.worker-v3.plist"),
                plist(with_authorisation),
            ),
            _ => (root.join("roost3-worker.service"), unit(with_authorisation)),
        };
        std::fs::write(&path, body).expect("the fixture can write its service definition");
        Self { path }
    }

    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).expect("the definition is still there")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The variable `roost_host::paths::worker_service_path` reads to find
    /// this definition instead of the host's own location.
    pub fn env_key(host: HostPlatform) -> &'static str {
        match host {
            HostPlatform::MacOs => "ROOST_WORKER_PLIST_ENV",
            _ => "ROOST_WORKER_UNIT_ENV",
        }
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

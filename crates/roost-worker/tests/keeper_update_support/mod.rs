// A support module that cannot say what it expected is not a support module;
// an integration-test module is its own crate, so the exemption is here.
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Compiled once per test binary, and each binary drives a different subset.
#![allow(dead_code)]

//! A scripted keeper host for the update-admission tests: a proof it answers
//! with, shutdowns it records, and a virtual clock the exit wait sleeps on, so
//! the 30 s exit budget is honoured without spending it. What calls it:
//! `keeper_update_action.rs`, `keeper_maintenance_admission.rs`. Depends on
//! `roost_worker::keeper_pool`'s host seam and the protocol's contract types.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, KEEPER_EMPTY_BINDING_DIGEST, KEEPER_RUNTIME_ABI, KeeperBinding,
    KeeperContractV1, KeeperUpdateAdmissionV1, keeper_binding_digest_input,
};
use roost_worker::keeper_pool::{
    EmptyKeeperShutdownExpectation, ExitWatch, HostFuture, KeeperRuntimeProbe, KeeperUpdateHost,
};
use sha2::{Digest, Sha256};

pub const SESSION_ID: &str = "10000000-0000-4000-8000-000000000001";
pub const KEEPER_EPOCH: &str = "20000000-0000-4000-8000-000000000001";
pub const KEEPER_PID: u32 = 4242;
pub const SOURCE_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
pub const TARGET_DIGEST: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// The digest a keeper holding `bindings` reports, computed here from the
/// protocol's canonical input rather than by the code under test.
pub fn digest_of(bindings: &[KeeperBinding]) -> String {
    let digest = Sha256::digest(keeper_binding_digest_input(bindings, &[]).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn binding(channel_id: u32, pid: i64) -> KeeperBinding {
    KeeperBinding { channel_id, pid }
}

pub fn contract(implementation_digest: &str, build_sha_char: char) -> KeeperContractV1 {
    KeeperContractV1 {
        protocol_version: 1,
        supported_features: vec!["keeper-contract-v1".to_owned()],
        required_features: vec!["keeper-contract-v1".to_owned()],
        implementation_digest: Some(implementation_digest.to_owned()),
        platform: "linux".to_owned(),
        arch: "x64".to_owned(),
        build_sha: build_sha_char.to_string().repeat(40),
        bun_abi: KEEPER_RUNTIME_ABI.to_owned(),
    }
}

/// v2 `update(requiredAction)`: a preserve keeps the source implementation, a
/// replace-empty moves to the target one.
pub fn update(preserve: bool, active: &[KeeperBinding]) -> JournaledKeeperUpdateV1 {
    let source = contract(SOURCE_DIGEST, 'a');
    let target = if preserve {
        contract(SOURCE_DIGEST, 'b')
    } else {
        contract(TARGET_DIGEST, 'b')
    };
    JournaledKeeperUpdateV1 {
        admission: KeeperUpdateAdmissionV1 {
            classification: if preserve {
                "worker-only-safe"
            } else {
                "keeper-restart-required"
            }
            .to_owned(),
            source_contract_digest: SOURCE_DIGEST.to_owned(),
            target_contract_digest: target.implementation_digest.clone().unwrap(),
            expected_keeper_pid: i64::from(KEEPER_PID),
            expected_keeper_epoch: KEEPER_EPOCH.to_owned(),
            expected_binding_digest: if preserve {
                digest_of(active)
            } else {
                KEEPER_EMPTY_BINDING_DIGEST.to_owned()
            },
            required_action: if preserve {
                "preserve"
            } else {
                "replace-empty"
            }
            .to_owned(),
        },
        source_contract: source,
        target_contract: target,
    }
}

/// An authenticated keeper proving `running` and holding `bindings`.
pub fn running(running: KeeperContractV1, bindings: &[KeeperBinding]) -> KeeperRuntimeProbe {
    KeeperRuntimeProbe {
        reachable: true,
        authenticated: true,
        contract: Some(running),
        keeper_pid: Some(KEEPER_PID),
        process_epoch: Some(KEEPER_EPOCH.to_owned()),
        bindings: Some(bindings.to_vec()),
        spawning_channels: Some(Vec::new()),
    }
}

/// How the scripted keeper leaves once a shutdown is accepted.
#[derive(Debug, Clone, Copy)]
pub enum Exit {
    /// Gone at the next look.
    Immediately,
    /// Still reachable for this long on the virtual clock.
    After(Duration),
    /// Never leaves.
    Never,
}

/// The scripted host. Every counter is what a test asserts a refusal did NOT do.
#[derive(Debug)]
pub struct ScriptedHost {
    proof: Mutex<KeeperRuntimeProbe>,
    exit: Exit,
    accepts_shutdown: bool,
    clock: Mutex<Duration>,
    stopped_at: Mutex<Option<Duration>>,
    pub probes: AtomicUsize,
    pub empty_shutdowns: Mutex<Vec<EmptyKeeperShutdownExpectation>>,
    pub forced_shutdowns: AtomicUsize,
}

impl ScriptedHost {
    pub fn new(proof: KeeperRuntimeProbe) -> Self {
        Self {
            proof: Mutex::new(proof),
            exit: Exit::Immediately,
            accepts_shutdown: true,
            clock: Mutex::new(Duration::ZERO),
            stopped_at: Mutex::new(None),
            probes: AtomicUsize::new(0),
            empty_shutdowns: Mutex::new(Vec::new()),
            forced_shutdowns: AtomicUsize::new(0),
        }
    }

    pub fn exiting(mut self, exit: Exit) -> Self {
        self.exit = exit;
        self
    }

    pub fn shutdown_calls(&self) -> usize {
        self.empty_shutdowns.lock().unwrap().len() + self.forced_shutdowns.load(Ordering::SeqCst)
    }

    pub fn elapsed_now(&self) -> Duration {
        *self.clock.lock().unwrap()
    }

    fn gone(&self) -> bool {
        let Some(stopped_at) = *self.stopped_at.lock().unwrap() else {
            return false;
        };
        match self.exit {
            Exit::Immediately => true,
            Exit::After(delay) => self.elapsed_now() >= stopped_at + delay,
            Exit::Never => false,
        }
    }

    fn stop(&self) -> bool {
        if self.accepts_shutdown {
            *self.stopped_at.lock().unwrap() = Some(self.elapsed_now());
        }
        self.accepts_shutdown
    }
}

impl ExitWatch for ScriptedHost {
    fn reachable(&self) -> HostFuture<'_, bool> {
        Box::pin(async move { !self.gone() && self.proof.lock().unwrap().reachable })
    }

    fn sleep(&self, duration: Duration) -> HostFuture<'_, ()> {
        Box::pin(async move {
            *self.clock.lock().unwrap() += duration;
        })
    }

    fn elapsed(&self) -> Duration {
        self.elapsed_now()
    }
}

impl KeeperUpdateHost for ScriptedHost {
    fn probe(&self) -> HostFuture<'_, KeeperRuntimeProbe> {
        Box::pin(async move {
            self.probes.fetch_add(1, Ordering::SeqCst);
            if self.gone() {
                return KeeperRuntimeProbe::unreachable();
            }
            self.proof.lock().unwrap().clone()
        })
    }

    fn shutdown_empty(&self, expected: EmptyKeeperShutdownExpectation) -> HostFuture<'_, bool> {
        Box::pin(async move {
            self.empty_shutdowns.lock().unwrap().push(expected);
            self.stop()
        })
    }

    fn shutdown_forced(&self) -> HostFuture<'_, bool> {
        Box::pin(async move {
            self.forced_shutdowns.fetch_add(1, Ordering::SeqCst);
            self.stop()
        })
    }
}

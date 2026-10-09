//! The heartbeat's reachable address: the live tailnet name wins, the
//! `ROOST_REACHABLE_ADDR` fallback answers when tailscale does not, a resolved
//! answer is reused for the five-minute TTL, and an empty answer is never
//! cached. Ports the `currentReachableAddr` cases of v2
//! `apps/worker/tests/transport/heartbeat.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use roost_host::MapEnv;
use roost_observability::clock::EventClock;
use roost_worker::runtime::bootstrap_redeem::ENV_REACHABLE_ADDR;
use roost_worker::runtime::reachable_addr::{REACHABLE_ADDR_TTL_MS, ReachableAddr};

#[derive(Debug, Default)]
struct SteppedClock(AtomicI64);

impl SteppedClock {
    fn advance(&self, by_ms: i64) {
        self.0.fetch_add(by_ms, Ordering::SeqCst);
    }
}

impl EventClock for SteppedClock {
    fn now_epoch_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    fn mono_ns(&self) -> u64 {
        0
    }
}

/// A tailnet resolver answering `answers[n]` on its n-th call (the last answer
/// repeating), counting its calls.
#[derive(Clone)]
struct ScriptedTailnet {
    answers: Arc<[&'static str]>,
    calls: Arc<AtomicUsize>,
}

impl ScriptedTailnet {
    fn answering(answers: &[&'static str]) -> Self {
        Self {
            answers: answers.into(),
            calls: Arc::default(),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn resolver(&self) -> Box<dyn Fn() -> String + Send> {
        let script = self.clone();
        Box::new(move || {
            let call = script.calls.fetch_add(1, Ordering::SeqCst);
            let last = script.answers.len().saturating_sub(1);
            script
                .answers
                .get(call.min(last))
                .copied()
                .unwrap_or_default()
                .to_owned()
        })
    }
}

fn reachable(tailnet: &ScriptedTailnet, env: MapEnv, clock: &Arc<SteppedClock>) -> ReachableAddr {
    let clock: Arc<dyn EventClock> = clock.clone();
    ReachableAddr::new(tailnet.resolver(), Box::new(env), clock)
}

#[test]
fn the_live_tailnet_name_wins_over_the_environment_fallback() {
    let tailnet = ScriptedTailnet::answering(&["mac-mini.tail1234.ts.net"]);
    let env = MapEnv::new().with(ENV_REACHABLE_ADDR, "fallback.example");
    let mut address = reachable(&tailnet, env, &Arc::default());
    assert_eq!(
        address.current().as_deref(),
        Some("mac-mini.tail1234.ts.net")
    );
}

#[test]
fn the_environment_answers_when_tailscale_does_not() {
    let tailnet = ScriptedTailnet::answering(&[""]);
    let env = MapEnv::new().with(ENV_REACHABLE_ADDR, " build-box.lan ");
    let mut address = reachable(&tailnet, env, &Arc::default());
    assert_eq!(address.current().as_deref(), Some("build-box.lan"));
    assert_eq!(tailnet.calls(), 1, "tailscale is asked first");
}

#[test]
fn nothing_resolved_is_none_and_is_not_cached() {
    let tailnet = ScriptedTailnet::answering(&["", "late.tail1234.ts.net"]);
    let clock = Arc::new(SteppedClock::default());
    let mut address = reachable(&tailnet, MapEnv::new(), &clock);
    assert_eq!(address.current(), None);
    assert_eq!(
        address.current().as_deref(),
        Some("late.tail1234.ts.net"),
        "an empty answer must not pin the next beat"
    );
    assert_eq!(tailnet.calls(), 2);
}

#[test]
fn a_resolved_name_is_reused_inside_the_ttl_and_refreshed_after_it() {
    let tailnet = ScriptedTailnet::answering(&["first.tail1234.ts.net", "second.tail1234.ts.net"]);
    let clock = Arc::new(SteppedClock::default());
    let mut address = reachable(&tailnet, MapEnv::new(), &clock);
    assert_eq!(address.current().as_deref(), Some("first.tail1234.ts.net"));

    clock.advance(REACHABLE_ADDR_TTL_MS - 1);
    assert_eq!(
        address.current().as_deref(),
        Some("first.tail1234.ts.net"),
        "inside the TTL the cached answer is reused"
    );
    assert_eq!(tailnet.calls(), 1, "no tailscale call inside the TTL");

    clock.advance(1);
    assert_eq!(address.current().as_deref(), Some("second.tail1234.ts.net"));
    assert_eq!(tailnet.calls(), 2);
}

#[test]
fn an_expired_name_that_no_longer_resolves_reports_unknown() {
    let tailnet = ScriptedTailnet::answering(&["first.tail1234.ts.net", ""]);
    let clock = Arc::new(SteppedClock::default());
    let mut address = reachable(&tailnet, MapEnv::new(), &clock);
    assert_eq!(address.current().as_deref(), Some("first.tail1234.ts.net"));
    clock.advance(REACHABLE_ADDR_TTL_MS);
    // An unknown beat leaves the field absent, which the coordinator answers
    // by keeping the prior row value rather than clearing it.
    assert_eq!(address.current(), None);
}

/// A Windows worker finds the Tailscale CLI the MSI installs, after an
/// explicit `ROOST_TAILSCALE_BIN`; without it the sidebar's hand-offs (VNC,
/// Remote Desktop) have no address and stay disabled.
#[test]
fn a_windows_worker_searches_the_tailscale_install_path() {
    use roost_host::HostPlatform;
    use roost_worker::host::tailnet::{TAILSCALE_BIN_ENV, tailscale_binary_candidates};

    let env = MapEnv::new().with(TAILSCALE_BIN_ENV, r"D:\tools\tailscale.exe");
    let candidates = tailscale_binary_candidates(HostPlatform::Windows, &env);
    assert_eq!(
        candidates,
        vec![
            r"D:\tools\tailscale.exe".to_string(),
            "tailscale.exe".to_string(),
            r"C:\Program Files\Tailscale\tailscale.exe".to_string(),
        ]
    );
}

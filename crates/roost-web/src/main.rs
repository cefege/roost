//! The wasm entry: scrub the address, ask the serving origin who it is, then
//! mount.
//!
//! Order is the whole file. The credential scrub runs before the application
//! graph is requested, because a bearer left in `location` is readable by every
//! module that loads afterwards — as a `Referer` on the first request, and in
//! any diagnostic that prints the address. The serving-origin probe runs before
//! the graph for the same class of reason: the coordinator a page dials is
//! decided from that answer, and a graph that started first would aim its first
//! RPC at the WORKER whenever a worker served this document (v2
//! `localBootstrap.ts:18-29`, `connect.ts:69-93`). Both are bounded, and both
//! fail closed onto the page's own origin.
//! This is a `main`, not a `#[wasm_bindgen(start)]`, because `dx` builds a
//! binary target: a wasm-bindgen start hook is only found in a library, and a
//! `cdylib`-only crate has no `main` for the native build of the same tree.

use roost_web::platform::browser::phase_marks::{PhaseName, mark_phase};
use roost_web::platform::{FragmentCredential, capture_and_scrub};

fn main() {
    roost_web::install_tracing();
    mark_phase(PhaseName::ModuleStart, &[]);
    #[cfg(target_arch = "wasm32")]
    roost_web::platform::browser::perf_counters::install_long_task_watch();
    #[cfg(target_arch = "wasm32")]
    roost_web::platform::browser::phase_marks::install_phase_timeline_member();
    // A wasm panic surfaces as `RuntimeError: unreachable` with no message; the
    // hook puts the message and location in the console, where the Playwright
    // oracle's page log and a user's bug report both read it.
    std::panic::set_hook(Box::new(|panic| {
        tracing::error!(target: "panic", %panic, "wasm panic");
    }));
    let credential = capture_and_scrub();
    tracing::info!(
        target: "auth",
        credential_captured = !matches!(credential, FragmentCredential::None),
        "address scrubbed before mount"
    );
    // Mounted from inside the probe's task so the graph is requested only after
    // the coordinator decision is final. `spawn_local` parks on the microtask
    // queue, which is what lets the probe's own await complete first.
    wasm_bindgen_futures::spawn_local(async move {
        roost_web::platform::door_probe::prime_serving_bootstrap().await;
        dioxus::launch(roost_web::App);
    });
}

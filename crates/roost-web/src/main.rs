//! The wasm entry: scrub the address, then mount.
//!
//! Order is the whole file. The credential scrub runs before the application
//! graph is requested, because a bearer left in `location` is readable by every
//! module that loads afterwards — as a `Referer` on the first request, and in
//! any diagnostic that prints the address.
//!
//! This is a `main`, not a `#[wasm_bindgen(start)]`, because `dx` builds a
//! binary target: a wasm-bindgen start hook is only found in a library, and a
//! `cdylib`-only crate has no `main` for the native build of the same tree.

use roost_web::platform::{FragmentCredential, capture_and_scrub};

fn main() {
    roost_web::install_tracing();
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
    dioxus::launch(roost_web::App);
}

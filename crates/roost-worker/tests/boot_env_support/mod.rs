//! One way to build a `MapEnv` that `WorkerBoot::resolve` will accept, shared
//! by every test binary that resolves a worker. Included by `#[path]`; nothing
//! in `src/` calls it, and no test may build a boot environment any other way.
//!
//! IT IS ITS OWN MODULE AND NOT PART OF `session_emit_support`, because burying
//! boot resolution under a session name is how the fifth site copies it. Four
//! binaries were already assembling their own — `retire_support`, the two
//! `worker_*` binaries, `browser_command_support` — and each spelled `HOME`
//! differently.
//!
//! `HOME` IS SET HERE BECAUSE `WorkerBoot::resolve` REFUSES WITHOUT IT, and it
//! refuses BEFORE it looks at the coordinator URL. That ordering is why three
//! test binaries hit the same `DataDir("HOME: …")` refusal independently and
//! each concluded its own fixture was wrong. It is a property of boot
//! resolution, not of any one test, which is the whole argument for one builder.
//!
//! The keeper executable is THIS TEST BINARY by default, and that is not a
//! convenience: the worker hashes whatever path it was given and compares the
//! digest against what the keeper at the endpoint reports, so a fixture keeper
//! is only admitted when it reports the digest of a file that exists. A test
//! that names a keeper which is not there gets refused at admission, which
//! reads as a keeper defect and is not one.

// Several binaries include this module and each calls a different subset of
// it, so a dead-code warning here is a statement about ONE binary and not
// about the fixture. Same asymmetry `credential_support::scratch` documents: a
// `pub` in a private support module is already unreachable outside the crate,
// so "make it private and see if it still builds" proves nothing here.
#![allow(dead_code)]

use roost_host::MapEnv;
use roost_host::paths::WORKER_LOG_DIR_ENV;
use roost_worker::runtime::boot::{
    ENV_KEEPER_EXECUTABLE, ENV_KEEPER_SOCKET, ENV_WORKER_KEY_PATH, WORKER_KEY_NAME, WorkerBoot,
};
use std::path::Path;

/// A boot environment that resolves, rooted at `root`.
///
/// Every path this sets is INSIDE `root`, so two fixtures given different
/// roots cannot see each other's key file or keeper socket — which is the
/// property a shared builder has to keep, since a builder that leaked a path
/// into the process environment would make the whole file's tests
/// order-dependent.
pub fn boot_env(root: &Path) -> MapEnv {
    let keeper_executable = std::env::current_exe().unwrap_or_else(|error| {
        panic!(
            "a running test binary has a path, and it is the only keeper a fixture \
             can name truthfully: {error}"
        )
    });
    MapEnv::new()
        // Before everything else, and unconditionally: without it `resolve`
        // refuses before reading the coordinator URL, and the refusal names
        // `DataDir("HOME: …")`, which points a reader at the data directory
        // rather than at the missing variable.
        .with("HOME", root.join("home").to_string_lossy().as_ref())
        // The install layout's own file name, so a test asserting the key a
        // boot reads is the one the layout names is not contradicted by its
        // fixture.
        .with(
            ENV_WORKER_KEY_PATH,
            root.join(WORKER_KEY_NAME).to_string_lossy().as_ref(),
        )
        .with(
            WORKER_LOG_DIR_ENV,
            root.join("logs").to_string_lossy().as_ref(),
        )
        .with(
            ENV_KEEPER_SOCKET,
            root.join("keeper.sock").to_string_lossy().as_ref(),
        )
        .with(
            ENV_KEEPER_EXECUTABLE,
            keeper_executable.to_string_lossy().as_ref(),
        )
}

/// The environment a boot resolves from, as the shared builder builds it.
///
/// A thin alias rather than a second construction, so a caller that wants the
/// resolved value and a caller that wants to keep tweaking the environment both
/// start from ONE description of it.
pub fn resolve_boot_env(root: &Path, platform: roost_host::HostPlatform) -> WorkerBoot {
    WorkerBoot::resolve(&boot_env(root), platform)
        .expect("a worker configuration resolves inside a scratch root")
}

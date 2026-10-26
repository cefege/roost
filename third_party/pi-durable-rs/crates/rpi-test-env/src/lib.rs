//! Test-only environment-variable mutation (edition 2024 gate).
//!
//! `std::env::set_var`/`remove_var` became `unsafe` in edition 2024. The rpi
//! test suites mutate the process environment extensively (~360 call sites);
//! this crate keeps that `unsafe` marking in **one** audited place instead of
//! scattering hundreds of anonymous `unsafe` blocks over test code.
//!
//! # Safety contract (the single justification for the internal `unsafe`)
//!
//! std marks env mutation unsafe because concurrent environment access can
//! be UB when **foreign (non-Rust) code** reads the environment without
//! synchronization (e.g. libc `getenv` racing on unix). The rpi test binaries
//! are pure Rust:
//!
//! - on unix, `std::env` access is serialized by a process-wide std-internal
//!   lock, so Rust-vs-Rust concurrency is data-race-free;
//! - on windows, std uses the Win32 environment API, which never hands out
//!   borrowed pointers.
//!
//! Child processes receive a copy of the environment at spawn time and do
//! not race with the parent.
//!
//! # Rules
//!
//! - **Test code only** (`#[cfg(test)]` modules, `tests/` binaries). Never
//!   link this crate from production code — production sites call
//!   `std::env::set_var`/`remove_var` directly inside a minimal `unsafe`
//!   block with a site-specific `// SAFETY:` comment.
//! - Do not use these helpers in a test process that hosts non-Rust code
//!   reading the environment concurrently; that is the one case the std
//!   `unsafe` marking protects against.

/// Test-scoped `std::env::set_var` (see the crate safety contract).
pub fn set_var<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    // SAFETY: test-only helper; the audited justification is the crate-level
    // safety contract (pure-Rust test processes; std-internal synchronization
    // on unix, pointer-free Win32 API on windows).
    unsafe { std::env::set_var(key, value) }
}

/// Test-scoped `std::env::remove_var` (see the crate safety contract).
pub fn remove_var<K: AsRef<std::ffi::OsStr>>(key: K) {
    // SAFETY: test-only helper; see the crate-level safety contract.
    unsafe { std::env::remove_var(key) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_remove_round_trip() {
        set_var("RPI_TEST_ENV_PROBE", "value-1");
        assert_eq!(
            std::env::var("RPI_TEST_ENV_PROBE").ok().as_deref(),
            Some("value-1")
        );
        remove_var("RPI_TEST_ENV_PROBE");
        assert!(std::env::var("RPI_TEST_ENV_PROBE").is_err());
    }
}

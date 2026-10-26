//! Vendored `quickjs.wasm` loading (port of `packages/codemode/src/wasm.ts`
//! @ abe508e1b) and the shared wasmtime engine.
//!
//! The binary is embedded into the release binary with `include_bytes!`
//! (avoids the upstream #10204 class of "packaged worker/wasm not found"),
//! so no filesystem path is ever needed. Compilation happens once, lazily,
//! on the first script execution.
//!
//! The module is driven directly through the `qjs_*` C ABI that the
//! `quickjs-wasi` package exports (the same ABI its own JS wrapper consumes);
//! no Node/Bun glue is involved.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// sha256 of the vendored `quickjs.wasm` (see `vendor/quickjs-wasi/PATCHES.md`).
pub const QUICKJS_WASM_SHA256: &str =
    "d4c9375f2b1ca4dc95f72c8aa2982a7a9951ac8011490d79c6582df732b4bbd9";

/// The embedded QuickJS wasm module (`quickjs-wasi@3.6.2`).
pub const QUICKJS_WASM: &[u8] = include_bytes!("../vendor/quickjs-wasi/quickjs.wasm");

/// Hard cap on the wasm linear memory (the JS heap limit is enforced by
/// QuickJS itself; this only bounds the underlying wasm allocation).
pub const WASM_MEMORY_LIMIT_BYTES: usize = 512 * 1024 * 1024;

/// Per-segment wasm fuel budget (CPU backstop). The budget is replenished at
/// every host-call bridge and settle boundary, so only a wasm-internal spin
/// that never returns to the host can exhaust it.
pub const FUEL_BUDGET: u64 = 100_000_000_000;

/// `MAX_STACK_SIZE` (quickjs-wasi: the binary has a 1 MiB linker-defined
/// stack; reserving half leaves headroom for native frames and
/// stack-overflow exception handling).
pub const MAX_STACK_SIZE: i32 = 524_288;

/// sha256 of the embedded module.
pub fn embedded_wasm_sha256() -> String {
    let mut hasher = Sha256::new();
    hasher.update(QUICKJS_WASM);
    format!("{:x}", hasher.finalize())
}

/// Whether the embedded module matches the pinned upstream artifact.
pub fn verify_embedded_wasm() -> bool {
    embedded_wasm_sha256() == QUICKJS_WASM_SHA256
}

/// Shared wasmtime engine (module compilation cache lives on the engine).
///
/// `consume_fuel` is enabled as a bounded-CPU backstop; the primary
/// interruption mechanism is `env.host_interrupt` (see
/// `runtime/worker.rs`). The wasm stack is larger than the default so
/// QuickJS's own `JS_SetMaxStackSize` check fires before wasmtime traps
/// (upstream quickjs-wasi throws a catchable `RangeError`).
pub fn engine() -> &'static wasmtime::Engine {
    static ENGINE: OnceLock<wasmtime::Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        // `max_wasm_stack` cannot exceed `async_stack_size` (wasmtime
        // validation); the codemode worker runs wasm synchronously, so the
        // larger reserve only needs to leave room for it.
        config.async_stack_size(4 * 1024 * 1024);
        config.max_wasm_stack(4 * 1024 * 1024);
        wasmtime::Engine::new(&config).expect("wasmtime engine config is valid")
    })
}

/// Compiled module cache (lazy: the first script execution compiles it).
static MODULE: OnceLock<Result<wasmtime::Module, String>> = OnceLock::new();

/// Compile the embedded module once. A failure is cached and returns the
/// error again on later calls.
pub fn compiled_module() -> Result<&'static wasmtime::Module, String> {
    MODULE
        .get_or_init(|| {
            wasmtime::Module::new(engine(), QUICKJS_WASM)
                .map_err(|error| format!("Failed to load QuickJS: {error}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Whether the module has been compiled yet (lazy-loading assertion surface
/// for the engine; the worker compiles it on the first script).
pub fn module_is_compiled() -> bool {
    MODULE.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G1/vendor gate: the embedded binary is byte-for-byte the pinned
    /// upstream artifact (provenance in `vendor/quickjs-wasi/PATCHES.md`).
    #[test]
    fn embedded_wasm_matches_the_pinned_sha256() {
        assert!(verify_embedded_wasm(), "{}", embedded_wasm_sha256());
        assert_eq!(QUICKJS_WASM.len(), 637_405);
    }

    #[test]
    fn module_compiles_once() {
        let module = compiled_module().expect("embedded module compiles");
        let again = compiled_module().expect("cached module");
        assert!(std::ptr::eq(module, again));
    }
}

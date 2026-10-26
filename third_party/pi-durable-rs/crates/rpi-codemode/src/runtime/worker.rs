//! Worker-thread entry: one worker runs one script inside a fresh QuickJS VM
//! (a separate wasm instance), relays tool calls and output to the host, and
//! reports the result (port of `packages/codemode/src/runtime/worker.ts` and
//! the host-side `Execution` lifecycle @ abe508e1b).
//!
//! The host side terminates the worker when the script settles, times out,
//! or is aborted; the worker exists so that a spinning script never blocks
//! the async host. The VM lives until the worker exits.

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Offset as _;
use serde_json::Value;
use wasmtime::{AsContextMut, Caller, Engine, Extern, Instance, Linker, Memory, Module, Store};

use crate::quickjs::{self, HostQjs, QjsFuncs, QjsInstance, QuickJsVm, Thrown};
use crate::runtime::prelude::PRELUDE_SOURCE;
use crate::runtime::protocol::{CallTarget, During, HostToWorker, WorkerToHost};
use crate::wasm::{FUEL_BUDGET, MAX_STACK_SIZE, WASM_MEMORY_LIMIT_BYTES, compiled_module, engine};

#[derive(Clone, Copy)]
pub struct DeterministicStart {
    pub started_at_ms: u64,
    pub seed: u32,
}

#[derive(Clone, Copy)]
struct VirtualClock {
    creating: bool,
    started_at_ms: u64,
    now_ms: u64,
    seed: u32,
    random_state: u32,
}

fn next_random(state: &mut u32) -> u32 {
    *state = state.wrapping_add(0x6d2b79f5);
    let mut value = (*state ^ (*state >> 15)).wrapping_mul(1 | *state);
    value = value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)) ^ value;
    value ^ (value >> 14)
}
/// Everything one execution needs to start.
pub struct WorkerInput {
    pub code: String,
    pub tools: Vec<(String, String, String)>,
    pub globals: Vec<(String, bool)>,
    pub store: serde_json::Map<String, Value>,
    pub memory_limit_bytes: Option<u64>,
    pub deterministic: Option<DeterministicStart>,
    pub interrupt: Arc<AtomicBool>,
    pub events: tokio::sync::mpsc::UnboundedSender<WorkerToHost>,
    pub replies: std::sync::mpsc::Receiver<HostToWorker>,
}

/// Host-call state of one worker VM.
struct WorkerHost {
    interrupt: Arc<AtomicBool>,
    events: tokio::sync::mpsc::UnboundedSender<WorkerToHost>,
    qjs: Option<QjsInstance>,
    done: bool,
    limiter: MemoryLimiter,
    clock: Option<Arc<Mutex<VirtualClock>>>,
    issued: u32,
    current_seq: u64,
}

impl HostQjs for WorkerHost {
    fn qjs_instance(&self) -> Option<QjsInstance> {
        self.qjs.clone()
    }
}

/// Hard cap on the wasm linear memory; the JS heap limit is enforced by
/// QuickJS itself (`qjs_set_memory_limit`).
struct MemoryLimiter;

impl wasmtime::ResourceLimiter for MemoryLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(desired <= WASM_MEMORY_LIMIT_BYTES.max(current))
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(true)
    }
}

/// Run one script on a dedicated thread. Never panics: all failures are
/// reported through the event channel.
pub fn run_worker(input: WorkerInput) {
    let module: Module = match compiled_module() {
        Ok(module) => module.clone(),
        Err(message) => {
            let _ = input.events.send(WorkerToHost::Crash { message });
            return;
        }
    };
    let clock = input.deterministic.map(|start| {
        Arc::new(Mutex::new(VirtualClock {
            creating: true,
            started_at_ms: start.started_at_ms,
            now_ms: start.started_at_ms,
            seed: start.seed,
            random_state: start.seed ^ 0x9e3779b9,
        }))
    });
    let mut store = Store::new(
        engine(),
        WorkerHost {
            interrupt: input.interrupt.clone(),
            events: input.events.clone(),
            qjs: None,
            done: false,
            limiter: MemoryLimiter,
            clock,
            issued: 0,
            current_seq: 0,
        },
    );
    store.limiter(|state| &mut state.limiter);
    // CPU backstop: the budget is replenished at every bridge/settle boundary
    // (see `host_call` and `run_script`), so only a wasm-internal spin can
    // exhaust it. QuickJS's own interrupt handler is the primary timeout
    // mechanism.
    let _ = store.set_fuel(FUEL_BUDGET);

    let mut linker: Linker<WorkerHost> = Linker::new(engine());
    if let Err(error) = define_imports(&mut linker) {
        let _ = input.events.send(WorkerToHost::Crash { message: error });
        return;
    }
    let instance: Instance = match linker.instantiate(&mut store, &module) {
        Ok(instance) => instance,
        Err(error) => {
            let _ = input.events.send(WorkerToHost::Crash {
                message: format!("Failed to start worker: {error}"),
            });
            return;
        }
    };
    let memory = match instance.get_memory(&mut store, "memory") {
        Some(memory) => memory,
        None => {
            let _ = input.events.send(WorkerToHost::Crash {
                message: "quickjs.wasm is missing the memory export".to_owned(),
            });
            return;
        }
    };
    let funcs = match QjsFuncs::new(&mut store, &instance) {
        Ok(funcs) => Arc::new(funcs),
        Err(message) => {
            let _ = input.events.send(WorkerToHost::Crash { message });
            return;
        }
    };
    store.data_mut().qjs = Some(QjsInstance {
        memory,
        funcs: funcs.clone(),
    });
    let mut vm = QuickJsVm {
        store,
        instance,
        memory,
        funcs,
    };

    if let Err(message) = run_script(&mut vm, input) {
        let _ = vm.store.data().events.send(WorkerToHost::Crash { message });
    }
}

/// UTC offset (seconds east) of `tz` at the instant split across `hi`:`lo`
/// (seconds since the epoch, high word first; the wasm ABI passes a 64-bit
/// value as two `i32`s). `0` when the instant is outside chrono's range.
fn timezone_offset_seconds_at<Tz: chrono::TimeZone>(tz: &Tz, hi: i32, lo: i32) -> i32 {
    let seconds = ((hi as i64) << 32) | (lo as u32 as i64);
    match tz.timestamp_opt(seconds, 0).single() {
        Some(time) => time.offset().fix().local_minus_utc(),
        None => 0,
    }
}

/// [`timezone_offset_seconds_at`] for the host timezone, matching upstream's
/// default `timezoneOffset: "host"` (`new Date(timeSecs * 1000)` in
/// quickjs-wasi's shim, negated to an east-of-UTC offset). `Date` is plain
/// JavaScript, so scripts observe this value (V16-07 review O-D).
fn host_timezone_offset_seconds(hi: i32, lo: i32) -> i32 {
    timezone_offset_seconds_at(&chrono::Local, hi, lo)
}

fn run_script(vm: &mut QuickJsVm<WorkerHost>, input: WorkerInput) -> Result<(), String> {
    quickjs::initialize(&mut vm.store, &vm.funcs)?;
    if let Some(limit) = input.memory_limit_bytes {
        quickjs::set_memory_limit(&mut vm.store, &vm.funcs, limit.min(i32::MAX as u64) as i32);
    }
    quickjs::set_max_stack_size(&mut vm.store, &vm.funcs, MAX_STACK_SIZE);
    quickjs::set_interrupt_handler(&mut vm.store, &vm.funcs);

    let prelude = {
        replenish_fuel(vm);
        match vm.eval(PRELUDE_SOURCE, "codemode-prelude.js") {
            Ok(value) => value,
            Err(thrown) => return crash_from(vm, thrown),
        }
    };
    let bridge = vm.new_host_function("bridge")?;
    let tools_json = serde_json::to_string(
        &input
            .tools
            .iter()
            .map(|(name, js_name, description)| {
                serde_json::json!({
                    "name": name,
                    "jsName": js_name,
                    "description": description,
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let globals_json = serde_json::to_string(
        &input
            .globals
            .iter()
            .map(|(name, spread)| serde_json::json!({ "name": name, "spread": spread }))
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let store_json = Value::Object(input.store.clone()).to_string();

    let tools_value = vm.new_string(&tools_json)?;
    let globals_value = vm.new_string(&globals_json)?;
    let store_value = vm.new_string(&store_json)?;
    let deterministic_value = vm.boolean(input.deterministic.is_some());
    let receiver = vm.undefined();
    let api = {
        replenish_fuel(vm);
        match vm.call(
            prelude,
            receiver,
            &[
                bridge,
                tools_value,
                globals_value,
                store_value,
                deterministic_value,
            ],
        ) {
            Ok(value) => value,
            Err(thrown) => return crash_from(vm, thrown),
        }
    };
    vm.free_value(prelude);
    vm.free_value(receiver);
    vm.free_value(bridge);
    vm.free_value(tools_value);
    vm.free_value(globals_value);
    vm.free_value(store_value);
    vm.free_value(deterministic_value);

    let settle = vm.get_prop(api, "settle")?;
    let run = vm.get_prop(api, "run")?;
    let stalled = vm.get_prop(api, "stalled")?;
    if let Some(clock) = vm.store.data().clock.as_ref() {
        clock
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .creating = false;
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(crate::wasm::QUICKJS_WASM);
        hasher.update(PRELUDE_SOURCE.as_bytes());
        hasher.update(FUEL_BUDGET.to_le_bytes());
        hasher.update((WASM_MEMORY_LIMIT_BYTES as u64).to_le_bytes());
        let fingerprint = format!(
            "quickjs-wasi 3.6.2, prelude {:x}, deterministic 1, stack {}, heap {}, budget {}",
            hasher.finalize(),
            MAX_STACK_SIZE,
            input
                .memory_limit_bytes
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            FUEL_BUDGET
        );
        let _ = vm
            .store
            .data()
            .events
            .send(WorkerToHost::Started { fingerprint });
    }

    // The prefix shares the first line with the script so reported line
    // numbers match the script as written.
    let source = format!("(async (tools, console) => {{{}\n}})", input.code);
    replenish_fuel(vm);
    let script = match vm.eval(&source, "codemode.js") {
        Ok(value) => value,
        Err(Thrown::Exception(exception)) => {
            let info = vm.describe_exception(exception);
            vm.free_value(exception);
            let state = vm.store.data();
            let during = state.clock.as_ref().map(|_| During {
                seq: state.current_seq,
                issued: state.issued,
            });
            let _ = state.events.send(WorkerToHost::Done {
                ok: false,
                value: None,
                writes: None,
                error: Some(info.json),
                during,
            });
            return Ok(());
        }
        Err(Thrown::Trap(message)) => return Err(message),
    };

    if let Err(thrown) = vm.call(run, api, &[script]) {
        return crash_from(vm, thrown);
    }
    vm.free_value(script);
    replenish_fuel(vm);
    if let Err(thrown) = drain(vm, api, stalled) {
        return crash_from(vm, thrown);
    }

    if vm.store.data().clock.is_some() && !vm.store.data().done {
        let state = vm.store.data();
        let _ = state.events.send(WorkerToHost::Idle {
            seq: 0,
            issued: state.issued,
        });
    }
    let deterministic = vm.store.data().clock.is_some();
    let mut next_seq = 1_u64;
    let mut early = BTreeMap::new();
    while !vm.store.data().done {
        let Ok(message) = input.replies.recv() else {
            break;
        };
        let HostToWorker::Result {
            seq,
            now_ms,
            id,
            ok,
            payload,
            properties,
        } = message
        else {
            break;
        };
        if deterministic {
            if seq < next_seq || early.contains_key(&seq) {
                return Err(format!("duplicate or stale delivery sequence {seq}"));
            }
            early.insert(
                seq,
                HostToWorker::Result {
                    seq,
                    now_ms,
                    id,
                    ok,
                    payload,
                    properties,
                },
            );
            while let Some(message) = early.remove(&next_seq) {
                apply_delivery(vm, settle, api, stalled, message, true)?;
                next_seq = next_seq.saturating_add(1);
                if vm.store.data().done {
                    break;
                }
            }
        } else {
            apply_delivery(
                vm,
                settle,
                api,
                stalled,
                HostToWorker::Result {
                    seq,
                    now_ms,
                    id,
                    ok,
                    payload,
                    properties,
                },
                false,
            )?;
        }
    }
    vm.free_value(settle);
    vm.free_value(run);
    vm.free_value(stalled);
    vm.free_value(api);
    Ok(())
}

fn apply_delivery(
    vm: &mut QuickJsVm<WorkerHost>,
    settle: u32,
    api: u32,
    stalled: u32,
    message: HostToWorker,
    deterministic: bool,
) -> Result<(), String> {
    let HostToWorker::Result {
        seq,
        now_ms,
        id,
        ok,
        payload,
        properties,
    } = message
    else {
        return Ok(());
    };
    if deterministic {
        if let Some(clock) = vm.store.data().clock.as_ref() {
            let mut clock = clock.lock().unwrap_or_else(|error| error.into_inner());
            clock.now_ms = clock.now_ms.max(now_ms);
        }
        vm.store.data_mut().current_seq = seq;
    }
    let id_value = vm.new_number(f64::from(id));
    let ok_value = vm.boolean(ok);
    let has_payload = payload.is_some();
    let payload_value = payload
        .as_deref()
        .map(|text| vm.new_string(text))
        .transpose()?
        .unwrap_or_else(|| vm.undefined());
    let has_properties = properties.is_some();
    let properties_value = properties
        .as_deref()
        .map(|text| vm.new_string(text))
        .transpose()?
        .unwrap_or_else(|| vm.undefined());
    let result = vm.call(
        settle,
        api,
        &[id_value, ok_value, payload_value, properties_value],
    );
    vm.free_value(id_value);
    vm.free_value(ok_value);
    if has_payload {
        vm.free_value(payload_value);
    }
    if has_properties {
        vm.free_value(properties_value);
    }
    replenish_fuel(vm);
    result.map_err(|thrown| format!("{thrown:?}"))?;
    drain(vm, api, stalled).map_err(|thrown| format!("{thrown:?}"))?;
    if deterministic && !vm.store.data().done {
        let state = vm.store.data();
        let _ = state.events.send(WorkerToHost::Idle {
            seq,
            issued: state.issued,
        });
    }
    Ok(())
}

/// Replenish the wasm fuel backstop at a boundary, unless the host has
/// abandoned this execution. After a timeout/abort the QuickJS interrupt
/// handler raises a catchable `InternalError`; a script that catches it and
/// keeps looping (for example `while(true){try{console.log("x")}catch{}}`)
/// would otherwise replenish its budget at every bridge call and spin the
/// worker thread forever (v0.1.6 review P1-4). Leaving one unit of fuel in
/// the store makes the next wasm instruction trap with an uncatchable
/// wasmtime error, so the thread exits.
fn replenish_fuel(vm: &mut QuickJsVm<WorkerHost>) {
    let interrupted = vm.store.data().interrupt.load(Ordering::SeqCst);
    let _ = vm.store.set_fuel(if interrupted { 1 } else { FUEL_BUDGET });
}

/// Run queued jobs, then fail a script that waits on nothing that can ever
/// resume it (`drain` in worker.ts:118-122).
fn drain(vm: &mut QuickJsVm<WorkerHost>, api: u32, stalled: u32) -> Result<(), Thrown> {
    let mut jobs = 0;
    while !vm.store.data().done && vm.is_job_pending() {
        vm.execute_pending_job()?;
        jobs += 1;
        if jobs % 50 == 0 {
            replenish_fuel(vm);
            if vm.store.data().interrupt.load(Ordering::SeqCst) {
                return Err(Thrown::Trap("execution interrupted".to_owned()));
            }
        }
    }
    replenish_fuel(vm);
    if vm.store.data().done {
        return Ok(());
    }
    let result = vm.call(stalled, api, &[])?;
    vm.free_value(result);
    Ok(())
}

fn crash_from(vm: &mut QuickJsVm<WorkerHost>, thrown: Thrown) -> Result<(), String> {
    match thrown {
        Thrown::Exception(exception) => {
            let info = vm.describe_exception(exception);
            vm.free_value(exception);
            let _ = vm.store.data().events.send(WorkerToHost::Crash {
                message: format!("{}: {}", info.name, info.message),
            });
            Ok(())
        }
        Thrown::Trap(message) => Err(message),
    }
}

// ---------------------------------------------------------------------------
// Imports: `env.*` (QuickJS host hooks) and a minimal `wasi_snapshot_preview1`
// (the worker discards engine output, worker.ts:32-44).
// ---------------------------------------------------------------------------

fn define_imports(linker: &mut Linker<WorkerHost>) -> Result<(), String> {
    linker
        .func_wrap("env", "host_call", host_call)
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "env",
            "host_interrupt",
            |mut caller: Caller<'_, WorkerHost>| -> i32 {
                let interrupted = caller.data().interrupt.load(Ordering::SeqCst);
                if interrupted {
                    // Make the interruption an uncatchable wasm trap at the
                    // next instruction instead of a catchable JS error
                    // (v0.1.6 review P1-4).
                    let _ = caller.as_context_mut().set_fuel(1);
                }
                i32::from(interrupted)
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "env",
            "host_promise_rejection",
            |mut caller: Caller<'_, WorkerHost>, promise: i32, reason: i32, _handled: i32| {
                // No handler: free the heap-allocated values
                // (quickjs-wasi src/index.ts:1041-1046).
                if let Some(qjs) = quickjs::caller_instance(&caller) {
                    quickjs::free_value(&mut caller, &qjs.funcs, promise as u32);
                    quickjs::free_value(&mut caller, &qjs.funcs, reason as u32);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "env",
            "host_module_normalize",
            |_: Caller<'_, WorkerHost>, _base: i32, name: i32| -> i32 {
                // No module loader: pass the specifier through unchanged.
                name
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "env",
            "host_module_load",
            |_: Caller<'_, WorkerHost>, _name: i32, _out_len: i32| -> i32 { 0 },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "env",
            "host_get_timezone_offset",
            |caller: Caller<'_, WorkerHost>, hi: i32, lo: i32| -> i32 {
                if caller.data().clock.is_some() {
                    0
                } else {
                    host_timezone_offset_seconds(hi, lo)
                }
            },
        )
        .map_err(|error| error.to_string())?;

    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |mut caller: Caller<'_, WorkerHost>,
             fd: i32,
             iovs: i32,
             iovs_len: i32,
             written: i32|
             -> i32 {
                // Engine diagnostics belong to the host application (a TUI):
                // discard every byte but report it as written so libc does not
                // retry (worker.ts:32-44).
                if fd != 1 && fd != 2 {
                    return 8; // BADF
                }
                let Some(memory) = caller_memory(&mut caller) else {
                    return 8;
                };
                let mut total: u32 = 0;
                for index in 0..iovs_len {
                    let base = iovs as u32 + (index as u32) * 8;
                    let Ok(buf) = quickjs::read_u32(&mut caller, &memory, base) else {
                        return 8;
                    };
                    let Ok(len) = quickjs::read_u32(&mut caller, &memory, base + 4) else {
                        return 8;
                    };
                    let _ = buf;
                    total = total.saturating_add(len);
                }
                if quickjs::write_u32(&mut caller, &memory, written as u32, total).is_err() {
                    return 8;
                }
                0
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_close",
            |_: Caller<'_, WorkerHost>, _fd: i32| -> i32 {
                52 // NOSYS
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_fdstat_get",
            |mut caller: Caller<'_, WorkerHost>, fd: i32, stat: i32| -> i32 {
                if fd != 1 && fd != 2 {
                    return 8; // BADF
                }
                let Some(memory) = caller_memory(&mut caller) else {
                    return 8;
                };
                let stat = stat as u32;
                if quickjs::write_u32(&mut caller, &memory, stat, 2).is_err() {
                    return 8;
                }
                if quickjs::write_u32(&mut caller, &memory, stat + 2, 0).is_err() {
                    return 8;
                }
                for offset in [8, 16] {
                    if quickjs::write_u32(&mut caller, &memory, stat + offset, 0).is_err()
                        || quickjs::write_u32(&mut caller, &memory, stat + offset + 4, 0).is_err()
                    {
                        return 8;
                    }
                }
                0
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "fd_seek",
            |_: Caller<'_, WorkerHost>,
             _fd: i32,
             _offset: i64,
             _whence: i32,
             _result: i32|
             -> i32 {
                52 // NOSYS
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "clock_time_get",
            |mut caller: Caller<'_, WorkerHost>,
             clock_id: i32,
             _precision: i64,
             result: i32|
             -> i32 {
                if clock_id != 0 && clock_id != 1 {
                    return 52; // NOSYS
                }
                let Some(memory) = caller_memory(&mut caller) else {
                    return 8;
                };
                let nanos = if let Some(clock) = caller.data().clock.as_ref() {
                    let state = clock.lock().unwrap_or_else(|error| error.into_inner());
                    if state.creating {
                        if clock_id == 0 {
                            u64::from(state.seed)
                                .saturating_add(1)
                                .saturating_mul(1_000)
                        } else {
                            0
                        }
                    } else if clock_id == 0 {
                        state.now_ms.saturating_mul(1_000_000)
                    } else {
                        state
                            .now_ms
                            .saturating_sub(state.started_at_ms)
                            .saturating_mul(1_000_000)
                    }
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_nanos() as u64)
                        .unwrap_or(0)
                };
                if quickjs::write_u32(&mut caller, &memory, result as u32, nanos as u32).is_err()
                    || quickjs::write_u32(
                        &mut caller,
                        &memory,
                        result as u32 + 4,
                        (nanos >> 32) as u32,
                    )
                    .is_err()
                {
                    return 8;
                }
                0
            },
        )
        .map_err(|error| error.to_string())?;
    linker
        .func_wrap(
            "wasi_snapshot_preview1",
            "random_get",
            |mut caller: Caller<'_, WorkerHost>, buf: i32, len: i32| -> i32 {
                // `RandomState` (SipHash-1-3 with per-thread, OS-seeded keys)
                // is not a CSPRNG. In quickjs-wasi 3.6.2 this import is
                // consumed once per VM by WASI libc init (`Math.random()`
                // seeds from `clock_time_get` instead), while the upstream
                // shim would use `crypto.getRandomValues`; the tradeoff and
                // the replacement path are documented in
                // `vendor/quickjs-wasi/PATCHES.md`.
                let Some(memory) = caller_memory(&mut caller) else {
                    return 8;
                };
                let mut bytes = vec![0u8; len.max(0) as usize];
                if let Some(clock) = caller.data().clock.as_ref() {
                    let mut state = clock.lock().unwrap_or_else(|error| error.into_inner());
                    for chunk in bytes.chunks_mut(4) {
                        let value = next_random(&mut state.random_state).to_le_bytes();
                        chunk.copy_from_slice(&value[..chunk.len()]);
                    }
                } else {
                    let mut counter: u64 = 0;
                    while (counter as usize) < bytes.len() {
                        let state = RandomState::new();
                        let mut hasher = state.build_hasher();
                        hasher.write_u64(counter);
                        hasher.write_u64(std::process::id() as u64);
                        let chunk = hasher.finish().to_le_bytes();
                        let start = counter as usize;
                        let end = (start + 8).min(bytes.len());
                        bytes[start..end].copy_from_slice(&chunk[..end - start]);
                        counter += 8;
                    }
                }
                match memory.write(&mut caller, buf.max(0) as usize, &bytes) {
                    Ok(()) => 0,
                    Err(_) => 8,
                }
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn caller_memory<W>(caller: &mut Caller<'_, W>) -> Option<Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(memory)) => Some(memory),
        _ => None,
    }
}

/// The generic `bridge` host function (worker.ts:65-100).
fn host_call(
    mut caller: Caller<'_, WorkerHost>,
    name_ptr: u32,
    name_len: u32,
    _this_ptr: u32,
    argc: u32,
    argv_ptr: u32,
) -> i32 {
    let Some(qjs) = quickjs::caller_instance(&caller) else {
        return 0;
    };
    let mut name_bytes = vec![0u8; name_len as usize];
    if qjs
        .memory
        .read(&caller, name_ptr as usize, &mut name_bytes)
        .is_err()
    {
        return 0;
    }
    let name = String::from_utf8_lossy(&name_bytes).into_owned();
    if name != "bridge" {
        return throw_host_error(
            &mut caller,
            &qjs,
            &format!("Host callback \"{name}\" is not registered"),
        );
    }
    let mut args = Vec::with_capacity(argc as usize);
    for index in 0..argc {
        match quickjs::read_u32(&mut caller, &qjs.memory, argv_ptr + index * 4) {
            Ok(value) => args.push(value),
            Err(_) => return 0,
        }
    }
    let Some(kind_value) = args.first().copied() else {
        return throw_host_error(&mut caller, &qjs, "bridge() expects a kind");
    };
    // Replenish the CPU backstop at the bridge boundary. A tiny budget
    // after abandonment forces the worker to trap out (P1-4).
    let interrupted = caller.data().interrupt.load(Ordering::SeqCst);
    let _ = caller
        .as_context_mut()
        .set_fuel(if interrupted { 1 } else { FUEL_BUDGET });
    let kind = match quickjs::read_string_value(&mut caller, &qjs.funcs, &qjs.memory, kind_value) {
        Ok(kind) => kind,
        Err(_) => return 0,
    };
    match kind.as_str() {
        "call" | "global" => {
            let id =
                quickjs::to_f64(&mut caller, &qjs.funcs, args.get(1).copied().unwrap_or(0)) as u32;
            let target_name = match quickjs::read_string_value(
                &mut caller,
                &qjs.funcs,
                &qjs.memory,
                args.get(2).copied().unwrap_or(0),
            ) {
                Ok(name) => name,
                Err(_) => return 0,
            };
            let args_json = match args.get(3).copied() {
                Some(value) if !quickjs::is_undefined(&mut caller, &qjs.funcs, value) => {
                    quickjs::read_string_value(&mut caller, &qjs.funcs, &qjs.memory, value).ok()
                }
                _ => None,
            };
            let target = if kind == "call" {
                CallTarget::Tool
            } else {
                CallTarget::Global
            };
            let issued = caller.data().issued.saturating_add(1);
            caller.data_mut().issued = issued;
            let _ = caller.data().events.send(WorkerToHost::Call {
                id,
                target,
                name: target_name,
                args: args_json,
            });
        }
        "output" => {
            let output_kind = match quickjs::read_string_value(
                &mut caller,
                &qjs.funcs,
                &qjs.memory,
                args.get(1).copied().unwrap_or(0),
            ) {
                Ok(kind) => kind,
                Err(_) => return 0,
            };
            let item = if output_kind == "image" {
                let data = match quickjs::read_string_value(
                    &mut caller,
                    &qjs.funcs,
                    &qjs.memory,
                    args.get(2).copied().unwrap_or(0),
                ) {
                    Ok(data) => data,
                    Err(_) => return 0,
                };
                let mime_type = match quickjs::read_string_value(
                    &mut caller,
                    &qjs.funcs,
                    &qjs.memory,
                    args.get(3).copied().unwrap_or(0),
                ) {
                    Ok(mime) => mime,
                    Err(_) => return 0,
                };
                crate::types::CodemodeOutputItem::Image { data, mime_type }
            } else {
                let text = match quickjs::read_string_value(
                    &mut caller,
                    &qjs.funcs,
                    &qjs.memory,
                    args.get(2).copied().unwrap_or(0),
                ) {
                    Ok(text) => text,
                    Err(_) => return 0,
                };
                crate::types::CodemodeOutputItem::Text {
                    text,
                    console: output_kind == "console",
                }
            };
            let _ = caller.data().events.send(WorkerToHost::Output(item));
        }
        "done" => {
            let ok = quickjs::to_bool(&mut caller, &qjs.funcs, args.get(1).copied().unwrap_or(0));
            if ok {
                let value = match args.get(2).copied() {
                    Some(value) if !quickjs::is_undefined(&mut caller, &qjs.funcs, value) => {
                        quickjs::read_string_value(&mut caller, &qjs.funcs, &qjs.memory, value).ok()
                    }
                    _ => None,
                };
                let writes = quickjs::read_string_value(
                    &mut caller,
                    &qjs.funcs,
                    &qjs.memory,
                    args.get(3).copied().unwrap_or(0),
                )
                .unwrap_or_else(|_| "[]".to_owned());
                let state = caller.data();
                let during = state.clock.as_ref().map(|_| During {
                    seq: state.current_seq,
                    issued: state.issued,
                });
                let _ = state.events.send(WorkerToHost::Done {
                    ok: true,
                    value,
                    writes: Some(writes),
                    error: None,
                    during,
                });
            } else {
                let error = quickjs::read_string_value(
                    &mut caller,
                    &qjs.funcs,
                    &qjs.memory,
                    args.get(2).copied().unwrap_or(0),
                )
                .unwrap_or_else(|_| "{\"message\":\"unknown script error\"}".to_owned());
                let state = caller.data();
                let during = state.clock.as_ref().map(|_| During {
                    seq: state.current_seq,
                    issued: state.issued,
                });
                let _ = state.events.send(WorkerToHost::Done {
                    ok: false,
                    value: None,
                    writes: None,
                    error: Some(error),
                    during,
                });
            }
            caller.data_mut().done = true;
        }
        _ => {}
    }
    // The trampoline takes ownership of the returned value; duplicate the
    // undefined singleton.
    let undefined = quickjs::undefined(&mut caller, &qjs.funcs);
    quickjs::dup(&mut caller, &qjs.funcs, undefined) as i32
}

fn throw_host_error(caller: &mut Caller<'_, WorkerHost>, qjs: &QjsInstance, message: &str) -> i32 {
    if let Ok(value) = quickjs::new_string(caller, &qjs.funcs, &qjs.memory, message) {
        quickjs::throw_value(caller, &qjs.funcs, value);
        quickjs::free_value(caller, &qjs.funcs, value);
    }
    0
}

/// The engine the worker thread runs on (kept public so the host side can
/// increment the epoch backstop on timeout).
pub fn engine_handle() -> &'static Engine {
    engine()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timezone_offset_combines_the_split_seconds_and_uses_the_zone() {
        let east = chrono::FixedOffset::east_opt(8 * 3600).expect("offset");
        assert_eq!(timezone_offset_seconds_at(&east, 0, 0), 8 * 3600);
        // -1 second since the epoch: hi = -1, lo = -1 in the wasm split.
        assert_eq!(timezone_offset_seconds_at(&east, -1, -1), 8 * 3600);
        // 2^33 + 1 seconds: hi = 2, lo = 1.
        assert_eq!(timezone_offset_seconds_at(&east, 2, 1), 8 * 3600);
        let west = chrono::FixedOffset::west_opt(5 * 3600).expect("offset");
        assert_eq!(timezone_offset_seconds_at(&west, 0, 0), -5 * 3600);
        // An instant beyond chrono's representable range falls back to UTC
        // instead of panicking.
        assert_eq!(timezone_offset_seconds_at(&east, i32::MAX, i32::MAX), 0);
    }
}

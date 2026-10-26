//! Minimal Rust binding over the `qjs_*` C ABI exported by the vendored
//! `quickjs.wasm` (the same ABI `quickjs-wasi`'s JS wrapper consumes).
//!
//! Only the subset the codemode prelude/host needs is bound: VM creation and
//! limits, string/number values, property access, function calls and eval,
//! host functions, the pending-job queue, and exceptions.
//!
//! Host callbacks (`env.host_call`) receive `this`/argument pointers owned by
//! the wasm-side C trampoline; those must never be freed here.

use std::sync::Arc;

use serde_json::{Map, Value};
use wasmtime::{AsContextMut, Caller, Instance, Memory, Store, TypedFunc};

/// Cached typed exports of one instantiated `quickjs.wasm`.
pub struct QjsFuncs {
    initialize: TypedFunc<(), ()>,
    qjs_init: TypedFunc<(), i32>,
    set_memory_limit: TypedFunc<i32, ()>,
    set_max_stack_size: TypedFunc<i32, ()>,
    set_interrupt_handler: TypedFunc<i32, ()>,
    set_promise_rejection_handler: TypedFunc<i32, ()>,
    wasm_malloc: TypedFunc<i32, i32>,
    wasm_free: TypedFunc<i32, ()>,
    new_string: TypedFunc<(i32, i32), i32>,
    new_number: TypedFunc<f64, i32>,
    get_undefined: TypedFunc<(), i32>,
    get_true: TypedFunc<(), i32>,
    get_false: TypedFunc<(), i32>,
    dup_value: TypedFunc<i32, i32>,
    free_value: TypedFunc<i32, ()>,
    is_exception: TypedFunc<i32, i32>,
    is_undefined: TypedFunc<i32, i32>,
    is_function: TypedFunc<i32, i32>,
    get_bool: TypedFunc<i32, i32>,
    get_float64: TypedFunc<i32, f64>,
    get_string_len: TypedFunc<(i32, i32), i32>,
    free_cstring: TypedFunc<i32, ()>,
    get_prop_string: TypedFunc<(i32, i32), i32>,
    set_prop_string: TypedFunc<(i32, i32, i32), i32>,
    call: TypedFunc<(i32, i32, i32, i32), i32>,
    new_host_function: TypedFunc<(i32, i32, i32), i32>,
    is_job_pending: TypedFunc<(), i32>,
    execute_pending_job: TypedFunc<(), i32>,
    get_exception: TypedFunc<(), i32>,
    throw: TypedFunc<i32, i32>,
    eval: TypedFunc<(i32, i32, i32, i32), i32>,
}

impl QjsFuncs {
    /// Resolve every needed export; a missing one means the vendored binary
    /// does not match the pinned ABI.
    pub fn new<W>(
        store: &mut impl AsContextMut<Data = W>,
        instance: &Instance,
    ) -> Result<Self, String> {
        macro_rules! typed {
            ($name:literal) => {
                instance
                    .get_typed_func(&mut *store, $name)
                    .map_err(|_| format!("quickjs.wasm is missing export `{}`", $name))?
            };
        }
        Ok(QjsFuncs {
            initialize: typed!("_initialize"),
            qjs_init: typed!("qjs_init"),
            set_memory_limit: typed!("qjs_set_memory_limit"),
            set_max_stack_size: typed!("qjs_set_max_stack_size"),
            set_interrupt_handler: typed!("qjs_set_interrupt_handler"),
            set_promise_rejection_handler: typed!("qjs_set_promise_rejection_handler"),
            wasm_malloc: typed!("wasm_malloc"),
            wasm_free: typed!("wasm_free"),
            new_string: typed!("qjs_new_string"),
            new_number: typed!("qjs_new_number"),
            get_undefined: typed!("qjs_get_undefined"),
            get_true: typed!("qjs_get_true"),
            get_false: typed!("qjs_get_false"),
            dup_value: typed!("qjs_dup_value"),
            free_value: typed!("qjs_free_value"),
            is_exception: typed!("qjs_is_exception"),
            is_undefined: typed!("qjs_is_undefined"),
            is_function: typed!("qjs_is_function"),
            get_bool: typed!("qjs_get_bool"),
            get_float64: typed!("qjs_get_float64"),
            get_string_len: typed!("qjs_get_string_len"),
            free_cstring: typed!("qjs_free_cstring"),
            get_prop_string: typed!("qjs_get_prop_string"),
            set_prop_string: typed!("qjs_set_prop_string"),
            call: typed!("qjs_call"),
            new_host_function: typed!("qjs_new_host_function"),
            is_job_pending: typed!("qjs_is_job_pending"),
            execute_pending_job: typed!("qjs_execute_pending_job"),
            get_exception: typed!("qjs_get_exception"),
            throw: typed!("qjs_throw"),
            eval: typed!("qjs_eval"),
        })
    }
}

/// The shared per-instance wasm state a host callback needs.
#[derive(Clone)]
pub struct QjsInstance {
    pub memory: Memory,
    pub funcs: Arc<QjsFuncs>,
}

/// Implemented by host-callback store data so [`caller_instance`] can reach
/// the module state.
pub trait HostQjs {
    fn qjs_instance(&self) -> Option<QjsInstance>;
}

/// Resolve the module state from a host-callback `Caller`.
pub fn caller_instance<W: HostQjs>(caller: &Caller<'_, W>) -> Option<QjsInstance> {
    caller.data().qjs_instance()
}

/// A thrown JS value, owned until [`free_value`] releases it, or a wasm-level
/// trap (epoch backstop, missing export) that never became a JS value.
#[derive(Debug, Clone)]
pub enum Thrown {
    Exception(u32),
    Trap(String),
}

impl Thrown {
    /// The raw exception value, when this is a JS exception.
    pub fn exception(&self) -> Option<u32> {
        match self {
            Thrown::Exception(value) => Some(*value),
            Thrown::Trap(_) => None,
        }
    }
}

/// Exception details read from a thrown value (`describeException` in
/// `runtime/worker.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionInfo {
    pub name: String,
    pub message: String,
    /// `{name?, message, stack}` JSON text, ready for the `done` message.
    pub json: String,
}

fn read_bytes<W>(
    store: &mut impl AsContextMut<Data = W>,
    memory: &Memory,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, String> {
    let mut buffer = vec![0u8; len as usize];
    memory
        .read(&*store, ptr as usize, &mut buffer)
        .map_err(|error| error.to_string())?;
    Ok(buffer)
}

fn write_bytes<W>(
    store: &mut impl AsContextMut<Data = W>,
    memory: &Memory,
    ptr: u32,
    bytes: &[u8],
) -> Result<(), String> {
    memory
        .write(&mut *store, ptr as usize, bytes)
        .map_err(|error| error.to_string())
}

/// `wasm_malloc` — returns 0 on failure.
pub fn malloc<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    size: u32,
) -> Result<u32, String> {
    funcs
        .wasm_malloc
        .call(store, size as i32)
        .map(|ptr| ptr as u32)
        .map_err(|error| error.to_string())
}

/// `wasm_free`.
pub fn free<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, ptr: u32) {
    let _ = funcs.wasm_free.call(store, ptr as i32);
}

/// Write a NUL-terminated string into guest memory; the caller frees the
/// returned pointer.
pub fn write_string<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    text: &str,
) -> Result<(u32, u32), String> {
    let bytes = text.as_bytes();
    let ptr = malloc(store, funcs, bytes.len() as u32 + 1)?;
    if ptr == 0 {
        return Err("wasm_malloc failed".to_owned());
    }
    write_bytes(store, memory, ptr, bytes)?;
    write_bytes(store, memory, ptr + bytes.len() as u32, &[0])?;
    Ok((ptr, bytes.len() as u32))
}

/// `qjs_get_string_len` + WTF-8 bytes → UTF-8 (lossy). The explicit length
/// keeps embedded NULs intact.
pub fn read_string_value<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    value: u32,
) -> Result<String, String> {
    let len_ptr = malloc(store, funcs, 4)?;
    if len_ptr == 0 {
        return Err("wasm_malloc failed".to_owned());
    }
    let result = (|| {
        let cstr = funcs
            .get_string_len
            .call(&mut *store, (value as i32, len_ptr as i32))
            .map_err(|error| error.to_string())? as u32;
        if cstr == 0 {
            return Ok("<null>".to_owned());
        }
        let len_bytes = read_bytes(store, memory, len_ptr, 4)?;
        let len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
        let bytes = read_bytes(store, memory, cstr, len)?;
        let _ = funcs.free_cstring.call(&mut *store, cstr as i32);
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })();
    free(store, funcs, len_ptr);
    result
}

/// `qjs_new_string` from a UTF-8 string; the caller owns the value.
pub fn new_string<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    text: &str,
) -> Result<u32, String> {
    let (ptr, len) = write_string(store, funcs, memory, text)?;
    let value = funcs
        .new_string
        .call(&mut *store, (ptr as i32, len as i32))
        .map_err(|error| error.to_string())? as u32;
    free(store, funcs, ptr);
    Ok(value)
}

pub fn new_number<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: f64) -> u32 {
    funcs.new_number.call(store, value).unwrap_or(0) as u32
}

pub fn undefined<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs) -> u32 {
    funcs.get_undefined.call(store, ()).unwrap_or(0) as u32
}

pub fn boolean<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: bool) -> u32 {
    let result = if value {
        &funcs.get_true
    } else {
        &funcs.get_false
    };
    result.call(store, ()).unwrap_or(0) as u32
}

pub fn dup<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: u32) -> u32 {
    funcs.dup_value.call(store, value as i32).unwrap_or(0) as u32
}

pub fn free_value<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: u32) {
    let _ = funcs.free_value.call(store, value as i32);
}

pub fn is_undefined<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    value: u32,
) -> bool {
    funcs.is_undefined.call(store, value as i32).unwrap_or(0) != 0
}

pub fn is_exception<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    value: u32,
) -> bool {
    funcs.is_exception.call(store, value as i32).unwrap_or(0) != 0
}

pub fn is_function<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    value: u32,
) -> bool {
    funcs.is_function.call(store, value as i32).unwrap_or(0) != 0
}

pub fn to_bool<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: u32) -> bool {
    funcs.get_bool.call(store, value as i32).unwrap_or(0) != 0
}

pub fn to_f64<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: u32) -> f64 {
    funcs.get_float64.call(store, value as i32).unwrap_or(0.0)
}

/// `qjs_get_prop_string`; the caller owns the result.
pub fn get_prop_string<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    object: u32,
    name: &str,
) -> Result<u32, String> {
    let (ptr, _) = write_string(store, funcs, memory, name)?;
    let value = funcs
        .get_prop_string
        .call(&mut *store, (object as i32, ptr as i32))
        .map_err(|error| error.to_string())? as u32;
    free(store, funcs, ptr);
    Ok(value)
}

/// `qjs_set_prop_string`; the store takes its own reference to `value`.
pub fn set_prop_string<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    object: u32,
    name: &str,
    value: u32,
) -> Result<(), String> {
    let (ptr, _) = write_string(store, funcs, memory, name)?;
    let result = funcs
        .set_prop_string
        .call(&mut *store, (object as i32, ptr as i32, value as i32))
        .map_err(|error| error.to_string());
    free(store, funcs, ptr);
    result.map(|_| ())
}

/// `qjs_new_host_function` for `name`; the host trampoline dispatches by that
/// name through `env.host_call`.
pub fn new_host_function<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    name: &str,
) -> Result<u32, String> {
    let (ptr, len) = write_string(store, funcs, memory, name)?;
    let value = funcs
        .new_host_function
        .call(&mut *store, (ptr as i32, len as i32, 0))
        .map_err(|error| error.to_string())? as u32;
    free(store, funcs, ptr);
    Ok(value)
}

/// `qjs_throw`.
pub fn throw_value<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, value: u32) {
    let _ = funcs.throw.call(store, value as i32);
}

/// Convert a raw call result into `Result<u32, Thrown>`: an exception
/// sentinel becomes the pending exception value.
fn finish_call<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    result: u32,
) -> Result<u32, Thrown> {
    if is_exception(store, funcs, result) {
        free_value(store, funcs, result);
        return Err(Thrown::Exception(get_exception(store, funcs)));
    }
    Ok(result)
}

/// Read `name`/`message`/`stack` off a thrown value and build the
/// `{name?, message, stack}` JSON text (`describeException`,
/// runtime/worker.ts:46-50).
pub fn describe_exception<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    memory: &Memory,
    thrown: u32,
) -> ExceptionInfo {
    let name_value = get_prop_string(store, funcs, memory, thrown, "name").unwrap_or(0);
    let message_value = get_prop_string(store, funcs, memory, thrown, "message").unwrap_or(0);
    let stack_value = get_prop_string(store, funcs, memory, thrown, "stack").unwrap_or(0);
    let name = if name_value != 0 && !is_undefined(store, funcs, name_value) {
        read_string_value(store, funcs, memory, name_value).unwrap_or_else(|_| "Error".to_owned())
    } else {
        "Error".to_owned()
    };
    let message = if message_value != 0 && !is_undefined(store, funcs, message_value) {
        read_string_value(store, funcs, memory, message_value).unwrap_or_default()
    } else {
        read_string_value(store, funcs, memory, thrown).unwrap_or_default()
    };
    let stack = if stack_value != 0 && !is_undefined(store, funcs, stack_value) {
        read_string_value(store, funcs, memory, stack_value)
            .ok()
            .map(|value| value.trim_end().to_owned())
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    for value in [name_value, message_value, stack_value] {
        if value != 0 {
            free_value(store, funcs, value);
        }
    }
    let head = if message.is_empty() {
        name.clone()
    } else {
        format!("{name}: {message}")
    };
    let mut map = Map::new();
    map.insert("name".to_owned(), Value::String(name.clone()));
    map.insert("message".to_owned(), Value::String(message.clone()));
    map.insert(
        "stack".to_owned(),
        Value::String(match &stack {
            Some(stack) => format!("{head}\n{stack}"),
            None => head,
        }),
    );
    ExceptionInfo {
        name,
        message,
        json: Value::Object(map).to_string(),
    }
}

/// `qjs_execute_pending_job` until the queue is empty.
pub fn execute_pending_jobs<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
) -> Result<usize, Thrown> {
    let mut count = 0usize;
    loop {
        if funcs.is_job_pending.call(&mut *store, ()).unwrap_or(0) == 0 {
            return Ok(count);
        }
        let result = funcs
            .execute_pending_job
            .call(&mut *store, ())
            .unwrap_or(-1);
        if result < 0 {
            return Err(Thrown::Exception(get_exception(store, funcs)));
        }
        count += 1;
    }
}

/// `qjs_is_job_pending`.
pub fn is_job_pending<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs) -> bool {
    funcs.is_job_pending.call(store, ()).unwrap_or(0) != 0
}

/// `qjs_get_exception` — the pending exception value (owned).
pub fn get_exception<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs) -> u32 {
    funcs.get_exception.call(store, ()).unwrap_or(0) as u32
}

/// Run the WASI reactor constructor and `qjs_init`.
pub fn initialize<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
) -> Result<(), String> {
    funcs
        .initialize
        .call(&mut *store, ())
        .map_err(|error| format!("Failed to initialize QuickJS runtime: {error}"))?;
    let result = funcs
        .qjs_init
        .call(&mut *store, ())
        .map_err(|error| format!("Failed to initialize QuickJS runtime: {error}"))?;
    if result != 0 {
        return Err("Failed to initialize QuickJS runtime".to_owned());
    }
    Ok(())
}

pub fn set_memory_limit<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs, bytes: i32) {
    let _ = funcs.set_memory_limit.call(store, bytes);
}

pub fn set_max_stack_size<W>(
    store: &mut impl AsContextMut<Data = W>,
    funcs: &QjsFuncs,
    bytes: i32,
) {
    let _ = funcs.set_max_stack_size.call(store, bytes);
}

pub fn set_interrupt_handler<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs) {
    let _ = funcs.set_interrupt_handler.call(store, 1);
}

pub fn set_promise_rejection_handler<W>(store: &mut impl AsContextMut<Data = W>, funcs: &QjsFuncs) {
    let _ = funcs.set_promise_rejection_handler.call(store, 1);
}

/// Find the module's memory export.
pub fn find_memory<W>(
    store: &mut impl AsContextMut<Data = W>,
    instance: &Instance,
) -> Result<Memory, String> {
    instance
        .get_memory(store, "memory")
        .ok_or_else(|| "quickjs.wasm is missing the memory export".to_owned())
}

/// Read a little-endian u32 from guest memory.
pub fn read_u32<W>(
    store: &mut impl AsContextMut<Data = W>,
    memory: &Memory,
    ptr: u32,
) -> Result<u32, String> {
    let bytes = read_bytes(store, memory, ptr, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Write a little-endian u32 into guest memory.
pub fn write_u32<W>(
    store: &mut impl AsContextMut<Data = W>,
    memory: &Memory,
    ptr: u32,
    value: u32,
) -> Result<(), String> {
    write_bytes(store, memory, ptr, &value.to_le_bytes())
}

/// A VM instance plus its cached exports, used by the worker thread.
pub struct QuickJsVm<W: 'static> {
    pub store: Store<W>,
    pub instance: Instance,
    pub memory: Memory,
    pub funcs: Arc<QjsFuncs>,
}

impl<W: 'static> QuickJsVm<W> {
    /// Evaluate `code` (`qjs_eval`).
    pub fn eval(&mut self, code: &str, filename: &str) -> Result<u32, Thrown> {
        let (code_ptr, code_len) =
            write_string(&mut self.store, &self.funcs, &self.memory, code).map_err(Thrown::Trap)?;
        let (name_ptr, _) = write_string(&mut self.store, &self.funcs, &self.memory, filename)
            .map_err(Thrown::Trap)?;
        let result = self.funcs.eval.call(
            &mut self.store,
            (code_ptr as i32, code_len as i32, name_ptr as i32, 0),
        );
        free(&mut self.store, &self.funcs, code_ptr);
        free(&mut self.store, &self.funcs, name_ptr);
        let result = result.map_err(|error| Thrown::Trap(error.to_string()))? as u32;
        finish_call(&mut self.store, &self.funcs, result)
    }

    /// Call `func` with `this` and `args` (`qjs_call`).
    pub fn call(&mut self, func: u32, this: u32, args: &[u32]) -> Result<u32, Thrown> {
        let argv_ptr = if args.is_empty() {
            0
        } else {
            let ptr = malloc(&mut self.store, &self.funcs, (args.len() * 4) as u32)
                .map_err(Thrown::Trap)?;
            if ptr == 0 {
                return Err(Thrown::Trap("wasm_malloc failed".to_owned()));
            }
            for (index, arg) in args.iter().enumerate() {
                let offset = ptr + (index * 4) as u32;
                if let Err(error) = write_u32(&mut self.store, &self.memory, offset, *arg) {
                    free(&mut self.store, &self.funcs, ptr);
                    return Err(Thrown::Trap(error));
                }
            }
            ptr
        };
        let result = self.funcs.call.call(
            &mut self.store,
            (func as i32, this as i32, args.len() as i32, argv_ptr as i32),
        );
        if argv_ptr != 0 {
            free(&mut self.store, &self.funcs, argv_ptr);
        }
        let result = result.map_err(|error| Thrown::Trap(error.to_string()))? as u32;
        finish_call(&mut self.store, &self.funcs, result)
    }

    pub fn get_prop(&mut self, object: u32, name: &str) -> Result<u32, String> {
        get_prop_string(&mut self.store, &self.funcs, &self.memory, object, name)
    }

    pub fn new_string(&mut self, text: &str) -> Result<u32, String> {
        new_string(&mut self.store, &self.funcs, &self.memory, text)
    }

    pub fn new_number(&mut self, value: f64) -> u32 {
        new_number(&mut self.store, &self.funcs, value)
    }

    pub fn undefined(&mut self) -> u32 {
        undefined(&mut self.store, &self.funcs)
    }

    pub fn boolean(&mut self, value: bool) -> u32 {
        boolean(&mut self.store, &self.funcs, value)
    }

    pub fn new_host_function(&mut self, name: &str) -> Result<u32, String> {
        new_host_function(&mut self.store, &self.funcs, &self.memory, name)
    }

    pub fn free_value(&mut self, value: u32) {
        free_value(&mut self.store, &self.funcs, value);
    }

    pub fn is_function(&mut self, value: u32) -> bool {
        is_function(&mut self.store, &self.funcs, value)
    }

    pub fn to_bool(&mut self, value: u32) -> bool {
        to_bool(&mut self.store, &self.funcs, value)
    }

    pub fn to_f64(&mut self, value: u32) -> f64 {
        to_f64(&mut self.store, &self.funcs, value)
    }

    pub fn read_string(&mut self, value: u32) -> Result<String, String> {
        read_string_value(&mut self.store, &self.funcs, &self.memory, value)
    }

    pub fn execute_pending_jobs(&mut self) -> Result<usize, Thrown> {
        execute_pending_jobs(&mut self.store, &self.funcs)
    }

    pub fn is_job_pending(&mut self) -> bool {
        is_job_pending(&mut self.store, &self.funcs)
    }

    pub fn execute_pending_job(&mut self) -> Result<(), Thrown> {
        if !is_job_pending(&mut self.store, &self.funcs) {
            return Ok(());
        }
        match self.funcs.execute_pending_job.call(&mut self.store, ()) {
            Ok(result) if result >= 0 => Ok(()),
            Ok(_) => Err(Thrown::Exception(get_exception(
                &mut self.store,
                &self.funcs,
            ))),
            Err(error) => Err(Thrown::Trap(error.to_string())),
        }
    }

    pub fn describe_exception(&mut self, thrown: u32) -> ExceptionInfo {
        describe_exception(&mut self.store, &self.funcs, &self.memory, thrown)
    }

    pub fn read_u32_at(&mut self, ptr: u32) -> Result<u32, String> {
        read_u32(&mut self.store, &self.memory, ptr)
    }
}

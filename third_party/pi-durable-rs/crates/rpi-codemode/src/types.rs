//! Public types of the codemode sandbox (port of
//! `packages/codemode/src/types.ts` @ abe508e1b).

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// A JSON Schema document. Only used to render declarations; values are not
/// validated against it.
pub type CodemodeJsonSchema = Value;

/// Context handed to an injected tool/global (`CodemodeToolContext`).
#[derive(Clone)]
pub struct CodemodeToolContext {
    /// Aborted when the script finishes (including unawaited calls), the
    /// execution times out, the caller aborts, or the sandbox is closed.
    pub signal: CancellationToken,
}

/// The future an injected tool/global returns.
pub type CodemodeToolFuture = BoxFuture<'static, Result<Value, String>>;

/// A function a script can call (`CodemodeTool`, types.ts:14-41). Tool and
/// global registrations share the shape; `spread` and `signature` are global
/// rendering concerns.
#[derive(Clone)]
pub struct CodemodeTool {
    /// The script calls tools as `tools.<id>(args)`, where `<id>` is the name
    /// with characters that are not valid in identifiers replaced by `_`
    /// (see [`crate::identifier::to_codemode_identifier`]), and also as
    /// `tools["<name>"](args)`. Globals are called as `<name>(args)` and must
    /// be identifiers, or `<namespace>.<member>`, which groups them into a
    /// frozen namespace object.
    pub name: String,
    /// Shown as a doc comment in declarations, and listed in `ALL_TOOLS`.
    pub description: Option<String>,
    /// Schema of the single argument; rendered as the parameter type.
    pub input_schema: Option<CodemodeJsonSchema>,
    /// Schema of the resolved value; rendered as the promise type.
    pub output_schema: Option<CodemodeJsonSchema>,
    /// Globals only: `execute` receives all call arguments as an array.
    pub spread: bool,
    /// Globals only: TypeScript parameter list and return type for
    /// declarations. Replaces the rendering from the schemas.
    pub signature: Option<String>,
    /// `args` is whatever the script passed, after a JSON round trip. The
    /// return value must be JSON-serializable; a returned `Err` surfaces in
    /// the script as an `Error` with the same message.
    pub execute: Arc<dyn Fn(Value, CodemodeToolContext) -> CodemodeToolFuture + Send + Sync>,
}

impl std::fmt::Debug for CodemodeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeTool")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl CodemodeTool {
    /// The declaration-only view used by the rendering helpers.
    pub fn info(&self) -> CodemodeToolInfo {
        CodemodeToolInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            output_schema: self.output_schema.clone(),
            signature: self.signature.clone(),
        }
    }
}

/// Declaration-relevant subset of [`CodemodeTool`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodemodeToolInfo {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<CodemodeJsonSchema>,
    pub output_schema: Option<CodemodeJsonSchema>,
    /// Globals only: explicit TypeScript signature (declarations.ts:132-147).
    pub signature: Option<String>,
}

/// One item of the script's output, in the order the script produced it:
/// `text()` and `console.*` produce text items (`console` true for
/// `console.*`), `image()` produces image items. Image `data` is base64
/// (types.ts:43-50 @ abe508e1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CodemodeOutputItem {
    #[serde(rename = "text")]
    Text {
        text: String,
        /// `console.*` items are marked so the caller can group them
        /// (`console: true`, types.ts:45-50 @ abe508e1b).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        console: bool,
    },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
}

/// `CodemodeCallStatus` (types.ts:51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeCallStatus {
    Ok,
    Error,
    Cancelled,
}

/// `CodemodeCall` (types.ts:51-55).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeCall {
    pub name: String,
    pub status: CodemodeCallStatus,
    pub duration_ms: f64,
}

/// `CodemodeErrorKind` (types.ts:57-65).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeErrorKind {
    /// The script threw or failed to parse. `name` and `stack` come from the
    /// script's error.
    Script,
    /// The overall deadline expired. The worker was terminated.
    Timeout,
    /// The caller's signal fired or the sandbox was closed.
    Aborted,
    /// The worker or VM failed outside the script's control (wasm trap,
    /// missing engine).
    Sandbox,
}

/// `CodemodeError` (types.ts:67-73).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct CodemodeError {
    pub kind: CodemodeErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

/// Keys the script changed with `store()`. Only successful executions
/// report writes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodemodeStoreWrites {
    pub set: serde_json::Map<String, Value>,
    #[serde(rename = "delete")]
    pub delete: Vec<String>,
}

/// `CodemodeResult` (types.ts:82-90). `output` is kept for failed
/// executions too, up to the failure. `value: None` is the script's
/// `undefined` (no `return`, or `exit()`); `Some(Value::Null)` is an explicit
/// `return null`.
#[derive(Debug, Clone, PartialEq)]
pub enum CodemodeResult {
    Ok {
        value: Option<Value>,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
        store_writes: CodemodeStoreWrites,
    },
    Err {
        error: CodemodeError,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
    },
}

/// Per-execution deadline (`timeoutMs` upstream; `Infinity` disables it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodemodeTimeout {
    /// `DEFAULT_TIMEOUT_MS` (300000).
    #[default]
    Default,
    /// `Number.POSITIVE_INFINITY` upstream: only ends when the script
    /// settles or is aborted.
    Infinite,
    Milliseconds(u64),
}

impl CodemodeTimeout {
    /// The effective deadline in milliseconds (`None` = no deadline).
    pub fn effective_ms(self) -> Option<u64> {
        match self {
            CodemodeTimeout::Default => Some(DEFAULT_TIMEOUT_MS),
            CodemodeTimeout::Infinite => None,
            CodemodeTimeout::Milliseconds(ms) => Some(ms),
        }
    }
}

/// `CodemodeSandboxOptions` (types.ts:92-125).
#[derive(Default)]
pub struct CodemodeSandboxOptions {
    pub tools: Vec<CodemodeTool>,
    /// Functions exposed as top-level identifiers instead of on `tools`, for
    /// host helpers such as attaching an image to the result. Names must be
    /// identifiers and may not shadow the built-in globals.
    pub globals: Vec<CodemodeTool>,
    /// Overall deadline per execution, including time spent in tools.
    /// Default: 300000 ms.
    pub timeout_ms: CodemodeTimeout,
    /// Maximum memory the QuickJS VM may allocate. Allocations beyond it fail
    /// inside the script as `InternalError: out of memory`. Default: none
    /// beyond wasm32's address space.
    pub memory_limit_bytes: Option<u64>,
}

/// `CodemodeExecuteOptions` (types.ts:127-136).
#[derive(Default)]
pub struct CodemodeExecuteOptions {
    pub signal: Option<CancellationToken>,
    /// Overrides the sandbox default for this execution.
    pub timeout_ms: Option<CodemodeTimeout>,
    /// Values the script reads with `load(key)`. The script's own `store()`
    /// calls come back as `result.storeWrites`; persisting them is up to the
    /// caller.
    pub store: Option<Value>,
}

/// Default overall deadline (`DEFAULT_TIMEOUT_MS`, host.ts:22).
pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// Store limits (prelude-source.ts MAX_STORE_VALUE_CHARS /
/// MAX_STORE_TOTAL_CHARS).
pub const MAX_STORE_VALUE_CHARS: usize = 256 * 1024;
pub const MAX_STORE_TOTAL_CHARS: usize = 1024 * 1024;

/// Output one script may produce with `text()`, `image()`, and `console.*`:
/// characters of text and base64 image data, and items. The host keeps all
/// output until the script ends, so without a limit a script that prints in
/// a loop grows the host's memory until it crashes. The item limit covers
/// loops that print empty strings (prelude-source.ts:39-42).
pub const MAX_OUTPUT_CHARS: usize = 16 * 1024 * 1024;
pub const MAX_OUTPUT_ITEMS: usize = 100_000;

/// Globals a host may not shadow (`RESERVED_GLOBALS`, host.ts:24-35).
pub const RESERVED_GLOBALS: [&str; 9] = [
    "tools",
    "ALL_TOOLS",
    "console",
    "text",
    "image",
    "exit",
    "globalThis",
    "store",
    "load",
];

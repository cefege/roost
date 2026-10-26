//! Sandboxed JavaScript execution where the only capability is calling
//! injected tools (port of `packages/codemode` @ abe508e1b).
//!
//! The engine is the vendored `quickjs.wasm` (quickjs-wasi 3.6.2) driven
//! through wasmtime (ADR-0034 decision 3 / ADR-0035): one OS thread and a
//! fresh QuickJS VM per script, tool calls relayed to the host as JSON
//! strings, and no timers, network, filesystem, modules, or host IO inside
//! the VM.

pub mod declarations;
pub mod identifier;
pub mod portable;
pub mod quickjs;
pub mod runtime;
pub mod source;
pub mod types;
pub mod wasm;

pub use declarations::{
    DEFAULT_INPUT_SCHEMA_MAX_CHARS, MCP_TYPESCRIPT_PREAMBLE, RenderDeclarationsOptions,
    mcp_structured_content_schema, render_declarations, render_tool_output_type,
    render_tool_sample, render_tool_signature, schema_to_type,
};
pub use identifier::to_codemode_identifier;
pub use runtime::host::CodemodeSandbox;
pub use runtime::protocol::{CallTarget, HostToWorker, WorkerToHost};
pub use source::{
    CODEMODE_OPTIONS_PREFIX, CODEMODE_SOURCE_GRAMMAR, CodemodeSourceError, CodemodeSourceOptions,
    ParsedCodemodeSource, parse_codemode_source,
};
pub use types::{
    CodemodeCall, CodemodeCallStatus, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions,
    CodemodeJsonSchema, CodemodeOutputItem, CodemodeResult, CodemodeSandboxOptions,
    CodemodeStoreWrites, CodemodeTimeout, CodemodeTool, CodemodeToolContext, CodemodeToolInfo,
    DEFAULT_TIMEOUT_MS, MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS,
    MAX_STORE_VALUE_CHARS, RESERVED_GLOBALS,
};
pub use wasm::{QUICKJS_WASM_SHA256, module_is_compiled, verify_embedded_wasm};

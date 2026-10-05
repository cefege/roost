//! The harness's one error type. Every module returns `BenchError`; only
//! `main.rs` turns it into an exit code. Depends on `thiserror` and nothing
//! else, so every failure names the step that failed and what it touched.

use std::path::PathBuf;

/// A step of the benchmark that could not complete.
#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("prerequisite missing: {0}")]
    Prerequisite(String),
    #[error("`{command}` exited with {status}")]
    CommandFailed { command: String, status: String },
    #[error("refusing to run: {0}")]
    Preflight(String),
    #[error("{what} timed out after {waited_ms} ms{detail}")]
    Timeout {
        what: String,
        waited_ms: u128,
        detail: String,
    },
    #[error("{stack} failed to boot: {reason}\n--- {log} (tail) ---\n{tail}")]
    Boot {
        stack: &'static str,
        reason: String,
        log: PathBuf,
        tail: String,
    },
    #[error("coordinator database {path}: {detail}")]
    Seed { path: PathBuf, detail: String },
    #[error("coordinator RPC {method}: {detail}")]
    Rpc {
        method: &'static str,
        detail: String,
    },
    #[error("browser: {0}")]
    Browser(String),
    #[error("{context}: {detail}")]
    Decode { context: String, detail: String },
}

impl BenchError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn timeout(what: impl Into<String>, waited: std::time::Duration) -> Self {
        Self::Timeout {
            what: what.into(),
            waited_ms: waited.as_millis(),
            detail: String::new(),
        }
    }

    pub fn browser(error: impl std::fmt::Display) -> Self {
        Self::Browser(error.to_string())
    }
}

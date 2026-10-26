//! Messages between the async host and the per-execution VM worker thread.

use crate::types::CodemodeOutputItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallTarget {
    Tool,
    Global,
}

#[derive(Debug, Clone)]
pub enum WorkerToHost {
    Started {
        fingerprint: String,
    },
    Call {
        id: u32,
        target: CallTarget,
        name: String,
        args: Option<String>,
    },
    Output(CodemodeOutputItem),
    Idle {
        seq: u64,
        issued: u32,
    },
    Done {
        ok: bool,
        value: Option<String>,
        writes: Option<String>,
        error: Option<String>,
        during: Option<During>,
    },
    Crash {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct During {
    pub seq: u64,
    pub issued: u32,
}

#[derive(Debug, Clone)]
pub enum HostToWorker {
    Result {
        seq: u64,
        now_ms: u64,
        id: u32,
        ok: bool,
        payload: Option<String>,
        properties: Option<String>,
    },
    Abort,
}

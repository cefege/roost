//! Stands in for the worker in the agent-chat RPC tests: answers `read` with a
//! hashline file, records every call, and reports no background jobs. The
//! parent module installs it as the coordinator's `ToolExecutor`.

use futures::future::BoxFuture;
use parking_lot::Mutex;
use roost_agent::{ToolCall, ToolExecutor, ToolOutcome};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Stands in for the worker: `read` answers with a hashline file.
#[derive(Debug, Default)]
pub(super) struct FakeWorker {
    pub(super) calls: Mutex<Vec<ToolCall>>,
}

impl ToolExecutor for FakeWorker {
    fn execute<'a>(
        &'a self,
        _worker_fp: &'a str,
        call: ToolCall,
        _out: mpsc::Sender<String>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>> {
        let content = if call.tool == "context_files" {
            r#"{"context":"","watchdog":""}"#.to_owned()
        } else {
            "[README.md#5BF9]\n1:hello".to_owned()
        };
        self.calls.lock().push(call);
        Box::pin(async move {
            Ok(ToolOutcome {
                is_error: false,
                content,
                details_json: "{}".into(),
            })
        })
    }

    fn close_conversation<'a>(
        &'a self,
        _worker_fp: &'a str,
        _conversation_id: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn detach_background_calls<'a>(&'a self, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn abort_background_jobs<'a>(&'a self, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn running_background_jobs<'a>(&'a self, _: &'a str) -> BoxFuture<'a, usize> {
        Box::pin(async { 0 })
    }
}

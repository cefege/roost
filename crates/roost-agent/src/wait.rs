//! The `wait` harness tool: holds a run open until one of its conversation's
//! background jobs settles, a message arrives, or the safety cap passes. A
//! settled job's result reaches the model as its own message, so this tool
//! only reports why it stopped waiting.

use std::pin::pin;
use std::time::Duration;

use crate::tool_round::{CallResult, CallScope};
use crate::traits::ToolOutcome;

/// Longest one `wait` blocks before it returns a still-running snapshot.
const WAIT_CAP: Duration = Duration::from_secs(30 * 60);

const NOTHING_TO_WAIT: &str =
    "Nothing to wait for: no background job you started is running. Messages arrive on their own.";
const WOKEN: &str = "Wait ended: a background job settled or a message arrived. Settled results arrive as their own messages.";
const CAP_REACHED: &str = "Wait limit reached; background work may still be running.";

/// Blocks until this conversation's background work or inbox changes, the run
/// is cancelled, or `WAIT_CAP` elapses.
pub(crate) async fn run(scope: &CallScope<'_>) -> CallResult {
    let conversation_id = scope.record.id.as_str();
    let arrival = scope.runtime.arrival_signal(conversation_id);
    let mut arrived = pin!(arrival.notified());
    // Enabling the waiter before the count is read keeps a job that settles
    // between the two from being missed.
    arrived.as_mut().enable();
    let running = scope
        .runtime
        .inner
        .tools
        .running_background_jobs(conversation_id)
        .await;
    if running == 0 {
        return CallResult::plain(text_outcome(NOTHING_TO_WAIT));
    }
    let content = tokio::select! {
        () = arrived.as_mut() => WOKEN,
        () = scope.cancel.cancelled() => return CallResult::plain(cancelled_outcome()),
        () = tokio::time::sleep(WAIT_CAP) => CAP_REACHED,
    };
    CallResult::plain(text_outcome(content))
}

fn text_outcome(content: &str) -> ToolOutcome {
    ToolOutcome {
        is_error: false,
        content: content.into(),
        details_json: String::new(),
    }
}

fn cancelled_outcome() -> ToolOutcome {
    ToolOutcome {
        is_error: false,
        content: "Wait cancelled.".into(),
        details_json: r#"{"cancelled":true}"#.into(),
    }
}

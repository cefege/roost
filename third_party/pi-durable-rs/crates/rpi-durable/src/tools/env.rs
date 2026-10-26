//! Environment injection point (port of `packages/durable/src/tools/env.ts`
//! @ `abe508e1b`).

use std::sync::Arc;

use crate::env::types::ExecutionEnv;
use crate::harness::types::{ToolExecuteError, ToolExecutionApi};
use crate::tools::tool_error;

/// The call's execution environment; a tool without one fails with an ordinary
/// error result.
pub fn require_env(api: &dyn ToolExecutionApi) -> Result<Arc<dyn ExecutionEnv>, ToolExecuteError> {
    api.env()
        .ok_or_else(|| tool_error("No execution environment is configured"))
}

//! Durable host driver for the portable WASM code-mode worker.
//!
//! Admission counts and results commit before effects are released or delivered;
//! replay follows result-delivery order and validates every acknowledged count.

mod calls;
mod output;
mod session;

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use rpi_codemode::portable::{
    CodemodeRun, DeclaredGlobal as PortableGlobal, DeclaredTool as PortableTool, Event,
    StartOptions,
};
use serde_json::Value;

use crate::chord::context::Context;
use crate::harness::codemode::driver::calls::{CallCompletion, HostCall};
use crate::harness::codemode::state::{DeclaredTool, JournalState, RunState};
use crate::harness::codemode::{error_result, state};
use crate::harness::types::{ToolExecutionApi, ToolExecutionResult};
use crate::ids::{ConversationId, TaskId};

use session::Driver;

const MAX_CONCURRENT_MODEL_CALLS: usize = 4;

type CallFuture = BoxFuture<'static, Result<CallCompletion, String>>;

pub(super) async fn execute(
    code: String,
    run_state: RunState,
    journal: JournalState,
    rerun: bool,
    api: Arc<dyn ToolExecutionApi>,
    context: Context,
) -> Result<ToolExecutionResult, crate::harness::types::ToolExecuteError> {
    let task_id = api.task_id();
    let conversation_id = api.conversation_id();
    let now_ms = clock_ms(api.as_ref())?;
    let remaining_timeout = match run_state.deadline_ms {
        Some(deadline) if deadline <= now_ms => {
            return persist_error(
                api,
                task_id,
                conversation_id,
                &context,
                "Script timed out: its deadline passed while it was interrupted.",
            )
            .await;
        }
        Some(deadline) => Some(Duration::from_millis(deadline - now_ms)),
        None => None,
    };
    let cancellation = context
        .abort_signal()
        .map(|signal| signal.token())
        .unwrap_or_default();
    let declared_tools = portable_tools(&run_state.tools);
    let declared_globals = run_state
        .globals
        .iter()
        .map(|global| PortableGlobal {
            name: global.name.clone(),
            spread: global.spread,
        })
        .collect();
    let store = serde_json::to_value(&run_state.store).map_err(super::tool_error)?;
    let options = StartOptions {
        started_at_ms: run_state.started_at_ms,
        seed: run_state.seed as u32,
        declared_tools,
        declared_globals,
        initial_store: store,
        cancellation_token: cancellation,
        remaining_timeout,
    };
    let mut run = match CodemodeRun::start(code, options).await {
        Ok(run) => run,
        Err(_)
            if context
                .abort_signal()
                .is_some_and(|signal| signal.is_cancelled()) =>
        {
            return Err(Box::new(crate::session::SessionError::Aborted));
        }
        Err(error) => {
            return persist_error(api, task_id, conversation_id, &context, &error).await;
        }
    };
    let fingerprint = match run.next_event().await {
        Ok(Event::Started { fingerprint }) => fingerprint,
        Ok(_) => {
            run.cancel();
            return persist_error(
                api,
                task_id,
                conversation_id,
                &context,
                "Code-mode worker did not report its sandbox fingerprint before execution.",
            )
            .await;
        }
        Err(_)
            if context
                .abort_signal()
                .is_some_and(|signal| signal.is_cancelled()) =>
        {
            run.cancel();
            return Err(Box::new(crate::session::SessionError::Aborted));
        }
        Err(error) => {
            run.cancel();
            return persist_error(api, task_id, conversation_id, &context, &error).await;
        }
    };
    if let Err(error) =
        state::persist_fingerprint(api.as_ref(), task_id, fingerprint, &context).await
    {
        run.cancel();
        return persist_error(api, task_id, conversation_id, &context, &error.to_string()).await;
    }

    let mut driver = Driver::new(run_state, journal, rerun, api, context);
    let mut running = FuturesUnordered::<CallFuture>::new();
    loop {
        tokio::select! {
            event = run.next_event() => {
                match event {
                    Ok(Event::Call { id, name, args, global }) => {
                        let call = HostCall { id, name, args, global };
                        if let Err(error) = driver.on_call(call, &mut run, &mut running).await {
                            run.cancel();
                            return driver.persist_failure(&error).await;
                        }
                    }
                    Ok(Event::Idle { seq, issued }) => {
                        if let Err(error) = driver.on_idle(seq, issued, &mut run, &mut running).await {
                            run.cancel();
                            return driver.persist_failure(&error).await;
                        }
                    }
                    Ok(Event::Output(_)) => {}
                    Ok(Event::Started { .. }) => {
                        run.cancel();
                        return driver.persist_failure("Code-mode worker emitted Started more than once.").await;
                    }
                    Ok(Event::Finished { result, during }) => {
                        if let Some(error) = during.and_then(|during| driver.finish_divergence(during)) {
                            run.cancel();
                            return driver.persist_failure(&error).await;
                        }
                        tracing::info!(
                            task_id = %task_id,
                            is_error = matches!(&result, rpi_codemode::CodemodeResult::Err { .. }),
                            "codemode_run_finished"
                        );
                        return output::complete(
                            result,
                            Arc::clone(&driver.api),
                            task_id,
                            conversation_id,
                            driver.run_state.started_at_ms,
                            driver.run_state.max_output_tokens,
                            driver.generated_images,
                            driver.run_state.store.clone(),
                            driver.context.clone(),
                        ).await;
                    }
                    Err(_) if driver.context.abort_signal().is_some_and(|signal| signal.is_cancelled()) => {
                        run.cancel();
                        return Err(Box::new(crate::session::SessionError::Aborted));
                    }
                    Err(error) => {
                        run.cancel();
                        return driver.persist_failure(&error).await;
                    }
                }
            }
            Some(completion) = running.next(), if !running.is_empty() => {
                match completion {
                    Ok(completion) => {
                        if let Err(error) = driver.on_completion(completion, &mut run, &mut running).await {
                            run.cancel();
                            return driver.persist_failure(&error).await;
                        }
                    }
                    Err(error) => {
                        run.cancel();
                        return driver.persist_failure(&error).await;
                    }
                }
            }
        }
    }
}

fn describe(call: &HostCall) -> String {
    if let Some(global) = &call.global {
        format!(
            "global {global}({})",
            call.args.as_ref().map(Value::to_string).unwrap_or_default()
        )
    } else {
        format!(
            "tools.{}({})",
            call.name,
            call.args.as_ref().map(Value::to_string).unwrap_or_default()
        )
    }
}

fn portable_tools(tools: &[DeclaredTool]) -> Vec<PortableTool> {
    tools
        .iter()
        .filter(|tool| !is_internal_model_tool(&tool.name))
        .map(|tool| PortableTool {
            name: tool.name.clone(),
            js_name: tool.js_name.clone(),
            description: tool.description.clone(),
        })
        .collect()
}

fn is_internal_model_tool(name: &str) -> bool {
    name == crate::harness::codemode::models::CLASSIFY_TOOL
        || name == crate::harness::codemode::models::GENERATE_IMAGES_TOOL
}

fn clock_ms(api: &dyn ToolExecutionApi) -> Result<u64, crate::harness::types::ToolExecuteError> {
    let now = api.now().map_err(super::tool_error)?;
    if !now.is_finite() || now < 0.0 || now > u64::MAX as f64 {
        return Err(super::tool_error(
            "Code-mode clock returned an invalid millisecond timestamp",
        ));
    }
    Ok(now as u64)
}

async fn persist_error(
    api: Arc<dyn ToolExecutionApi>,
    task_id: TaskId,
    conversation_id: ConversationId,
    context: &Context,
    message: &str,
) -> Result<ToolExecutionResult, crate::harness::types::ToolExecuteError> {
    state::persist_completion(
        api.as_ref(),
        task_id,
        conversation_id,
        error_result(message),
        None,
        context.clone(),
    )
    .await
}

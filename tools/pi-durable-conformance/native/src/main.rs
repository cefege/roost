mod provider;
mod queue_tasks;
mod recovery_traces;
mod scenarios;

use pi_durable_conformance_native::{compare_trace, load_fixture};
use provider::{OracleProvider, message};
use rpi_ai::types::StopReason;
use rpi_durable::chord::context::BACKGROUND_CONTEXT;
use rpi_durable::harness::{
    AgentChange, HarnessOptions, InputSubmissionDraft, ModelRef, RegistryImpl, SubmissionDraft,
    builtin_tasks, open_harness,
};
use rpi_durable::storage::MemoryStorage;
use rpi_durable::types::Storage;
use serde_json::{Value, json};
use std::env;
use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = env::args().collect();
    let (scenario, fixture_path) = match arguments.as_slice() {
        [_, scenario] => (scenario.as_str(), None),
        [_, scenario, flag, fixture] if flag == "--compare" => {
            (scenario.as_str(), Some(fixture.as_str()))
        }
        _ => {
            return Err(
                "usage: pi-durable-conformance-native <scenario> [--compare fixture.json]".into(),
            );
        }
    };
    let actual = normalize_trace(match scenario {
        "plain-dedup-context-usage" => plain_dedup_context_usage().await?,
        "tool-round" => scenarios::tool_round().await?,
        "sqlite-checkpoint-reopen-dedup" => scenarios::sqlite_checkpoint_reopen().await?,
        "fork-document-policies" => queue_tasks::fork_document_policies().await?,
        "durable-sleep-wake" => queue_tasks::durable_sleep_wake().await?,
        "generic-task-ownership-abort" => queue_tasks::generic_task_ownership_abort().await?,
        "busy-steer-followup-reject-withdraw" => {
            queue_tasks::busy_steer_followup_reject_withdraw().await?
        }
        "unsafe-tool-sigkill-recovery"
        | "nested-tool-identity-replay-structured-error"
        | "read-only-observation"
        | "deferred-document-progress" => recovery_traces::run(scenario).await?,
        other => {
            return Err(
                format!("native producer has no scenario implementation for {other}").into(),
            );
        }
    });
    if let Some(fixture_path) = fixture_path {
        let fixture = load_fixture(fixture_path)?;
        compare_trace(&fixture, &actual)?;
        println!("native {scenario} trace matches pinned upstream fixture");
    } else {
        println!("{}", serde_json::to_string_pretty(&actual)?);
    }
    Ok(())
}
fn normalize_trace(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(normalize_trace).collect()),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "timestamp" | "durationMs" | "startedAt" | "endedAt"
                    )
                })
                .map(|(key, value)| (key, normalize_trace(value)))
                .collect(),
        ),
        Value::String(value) if is_generated_faux_api_id(&value) => {
            Value::String("faux:<generated>".to_string())
        }
        other => other,
    }
}

fn is_generated_faux_api_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("faux:") else {
        return false;
    };
    let Some((timestamp, random)) = rest.split_once(':') else {
        return false;
    };
    !timestamp.is_empty()
        && timestamp.bytes().all(|byte| byte.is_ascii_digit())
        && !random.is_empty()
        && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

async fn plain_dedup_context_usage() -> Result<Value, Box<dyn Error>> {
    let (models, provider) =
        OracleProvider::new([message("oracle answer", StopReason::Stop, None)]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    let options = HarnessOptions {
        models,
        registry: Arc::clone(&registry) as Arc<dyn rpi_durable::harness::RegistryReader>,
        settings: None,
        env: None,
        conversation_created: None,
        now: None,
        on_report: None,
        read_only: false,
        set_timer: None,
        on_wake: None,
    };
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let context = BACKGROUND_CONTEXT.clone();
    let harness = open_harness(storage, options, &context).await?;
    let conversation = harness
        .root(
            &context,
            rpi_durable::harness::RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_string(),
                        model_id: "faux-1".to_string(),
                    })),
                    ..AgentChange::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    let draft = || {
        SubmissionDraft::Input(InputSubmissionDraft {
            request_id: Some("request-1".to_string()),
            content: rpi_ai::types::UserContent::Text("hello".to_string()),
            when_busy: None,
        })
    };
    let first = conversation.submit(draft(), &context).await?;
    let duplicate = conversation.submit(draft(), &context).await?;
    first.wait(&context).await?;
    let entries = conversation
        .entries(
            rpi_durable::types::EntryQuery {
                conversation_id: conversation.id(),
                min_entry_id: None,
                max_entry_id: None,
                order: Some(rpi_durable::types::ScanOrder::Ascending),
            },
            1_000,
            None,
            &context,
        )
        .await?
        .items;
    let requests = provider
        .requests
        .lock()
        .map_err(|_| "provider request capture poisoned")?
        .clone();
    if let Some(error) = provider
        .request_error
        .lock()
        .map_err(|_| "provider error capture poisoned")?
        .as_ref()
    {
        return Err(error.clone().into());
    }
    let trace = json!({
        "name": "plain-dedup-context-usage",
        "requestIds": [first.id(), duplicate.id()],
        "requestCount": requests.len(),
        "requests": requests,
        "entries": entries,
        "usage": harness.usage(&context).await?,
    });
    harness.close(&context).await?;
    Ok(trace)
}

//! Busy-conversation queue and admission oracle scenario.
//! Called by `queue_tasks` and built from its shared provider and harness helpers.
//! Exercises real submission admission, placement, and withdrawal behavior.
use super::*;
use crate::provider::FauxUsageEstimator;
// Gated model provider for admission into a generation whose first response is
// deliberately held. Its events still flow through rpi-ai's normal stream API.
struct GatedProvider {
    model: rpi_ai::types::Model,
    auth: rpi_ai::auth::ProviderAuth,
    gate: Arc<Semaphore>,
    reached: Arc<Notify>,
    calls: AtomicUsize,
    requests: Arc<Mutex<Vec<Value>>>,
    usage: FauxUsageEstimator,
}
struct QueueAuth;

#[async_trait]
impl rpi_ai::auth::ApiKeyAuth for QueueAuth {
    fn name(&self) -> &str {
        "pi-durable-conformance"
    }
    async fn resolve(
        &self,
        _context: &dyn rpi_ai::auth::AuthContext,
        _credential: Option<&rpi_ai::auth::ApiKeyCredential>,
    ) -> Result<Option<rpi_ai::auth::AuthResult>, rpi_ai::auth::ModelsError> {
        Ok(Some(rpi_ai::auth::AuthResult {
            auth: rpi_ai::auth::ModelAuth {
                api_key: None,
                headers: None,
                base_url: None,
            },
            env: None,
            source: None,
        }))
    }
}

impl GatedProvider {
    fn new() -> (rpi_ai::models::Models, Arc<Self>) {
        use rpi_ai::types::{ApiKind, InputModality, ModelCost};
        let provider = Arc::new(Self {
            model: rpi_ai::types::Model {
                id: "faux-1".into(),
                name: "Faux 1".into(),
                api: ApiKind::from("faux"),
                provider: "faux".into(),
                base_url: "http://localhost".into(),
                reasoning: false,
                thinking_level_map: None,
                input: vec![InputModality::Text],
                input_limits: None,
                cost: ModelCost::default(),
                prompt_cache: None,
                context_window: 200_000,
                max_tokens: 4_096,
                sampling_params: None,
                sampling_params_by_thinking_level: None,
                headers: None,
                compat: None,
            },
            auth: rpi_ai::auth::ProviderAuth {
                api_key: Some(Arc::new(QueueAuth)),
                oauth: None,
            },
            gate: Arc::new(Semaphore::new(0)),
            reached: Arc::new(Notify::new()),
            calls: AtomicUsize::new(0),
            requests: Arc::new(Mutex::new(Vec::new())),
            usage: FauxUsageEstimator::default(),
        });
        let models = rpi_ai::models::Models::new(None);
        models.set_provider(Arc::clone(&provider) as Arc<dyn rpi_ai::models::Provider>);
        (models, provider)
    }
    fn stream_response(
        &self,
        transcript: &rpi_ai::types::TranscriptContext,
        options: Option<&rpi_ai::types::StreamOptions>,
    ) -> rpi_ai::utils::event_stream::AssistantMessageEventStream {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(serde_json::to_value(&transcript.messages).unwrap_or(Value::Null));
        }
        let mut response = message(
            if call == 0 { "first" } else { "successor-1" },
            StopReason::Stop,
            None,
        );
        self.usage.apply(&mut response, transcript, options);
        let gated = call == 0;
        let gate = Arc::clone(&self.gate);
        let reached = Arc::clone(&self.reached);
        let stream = rpi_ai::utils::event_stream::AssistantMessageEventStream::new();
        let producer = stream.clone();
        tokio::spawn(async move {
            if gated {
                reached.notify_one();
                if let Ok(permit) = gate.acquire().await {
                    permit.forget();
                }
            }
            let mut partial = response.clone();
            partial.stop_reason = StopReason::Pending;
            producer.push(rpi_ai::types::StreamEvent::Start {
                partial: Arc::new(partial),
            });
            producer.push(rpi_ai::types::StreamEvent::Done {
                reason: rpi_ai::types::DoneReason::Stop,
                message: response,
            });
        });
        stream
    }
}

impl rpi_ai::models::Provider for GatedProvider {
    fn id(&self) -> &str {
        "faux"
    }
    fn name(&self) -> &str {
        "Faux"
    }
    fn base_url(&self) -> Option<&str> {
        Some("http://localhost")
    }
    fn headers(&self) -> Option<&rpi_ai::types::ProviderHeaders> {
        None
    }
    fn auth(&self) -> &rpi_ai::auth::ProviderAuth {
        &self.auth
    }
    fn get_models(&self) -> Vec<rpi_ai::types::Model> {
        vec![self.model.clone()]
    }
    fn stream(
        &self,
        _model: &rpi_ai::types::Model,
        context: &rpi_ai::types::TranscriptContext,
        options: Option<rpi_ai::types::StreamOptions>,
    ) -> rpi_ai::utils::event_stream::AssistantMessageEventStream {
        self.stream_response(context, options.as_ref())
    }
    fn stream_simple(
        &self,
        _model: &rpi_ai::types::Model,
        context: &rpi_ai::types::TranscriptContext,
        options: Option<rpi_ai::types::SimpleStreamOptions>,
    ) -> Result<rpi_ai::utils::event_stream::AssistantMessageEventStream, String> {
        Ok(self.stream_response(context, options.as_ref().map(|options| &options.stream)))
    }
}

fn input_draft(
    value: &str,
    when_busy: Option<rpi_durable::harness::WhenBusy>,
) -> rpi_durable::harness::SubmissionDraft {
    rpi_durable::harness::SubmissionDraft::Input(rpi_durable::harness::InputSubmissionDraft {
        request_id: None,
        content: rpi_ai::types::UserContent::Text(value.into()),
        when_busy,
    })
}
/// Verify steer/follow-up placements, busy rejection, and queued withdrawal.
pub async fn busy_steer_followup_reject_withdraw() -> ScenarioResult<Value> {
    let (models, provider) = GatedProvider::new();
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    let harness = open_harness(
        Arc::new(MemoryStorage::new()),
        options(models, registry),
        context(),
    )
    .await?;
    let conversation = harness
        .root(
            context(),
            rpi_durable::harness::RootOptions {
                agent: Some(rpi_durable::harness::AgentChange {
                    model: Some(Some(rpi_durable::harness::ModelRef {
                        provider: "faux".into(),
                        model_id: "faux-1".into(),
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    harness.resume()?;
    let first_request = provider.reached.notified();
    let first = conversation
        .submit(input_draft("start", None), context())
        .await?;
    tokio::time::timeout(Duration::from_secs(5), first_request)
        .await
        .map_err(|_| "timed out waiting for first model request")?;
    let steer = conversation
        .submit(
            input_draft("steer", Some(rpi_durable::harness::WhenBusy::Steer)),
            context(),
        )
        .await?;
    let follow_up = conversation
        .submit(input_draft("follow", None), context())
        .await?;
    let withdrawn = conversation
        .submit(input_draft("withdraw me", None), context())
        .await?;
    let reject = match conversation
        .submit(
            input_draft("reject", Some(rpi_durable::harness::WhenBusy::Reject)),
            context(),
        )
        .await
    {
        Ok(_) => json!({"accepted":true}),
        Err(error) => {
            let busy_message = format!("Conversation {} is busy", conversation.id());
            if error.to_string() != busy_message {
                return Err(error.into());
            }
            json!({"accepted":false,"name":"ConversationBusy","message":busy_message})
        }
    };
    let mut queued_statuses = Vec::new();
    for submission in [steer.as_ref(), follow_up.as_ref(), withdrawn.as_ref()] {
        queued_statuses.push(serde_json::to_value(submission.status(context()).await?)?);
    }
    let inbox_while_busy = harness
        .session()
        .snapshot(
            &rpi_durable::harness::inbox::INBOX_DOC,
            DocOwner::Conversation(conversation.id()),
            context(),
        )
        .await?
        .and_then(|object| object.get("items").cloned())
        .unwrap_or(JsonValue::Null);
    let withdrawal = match withdrawn.abort(context()).await? {
        rpi_durable::harness::SubmissionAbort::Aborted => "aborted",
        rpi_durable::harness::SubmissionAbort::AlreadyPlaced => "alreadyPlaced",
        rpi_durable::harness::SubmissionAbort::Settled => "settled",
    };
    provider.gate.add_permits(1);
    first.wait(context()).await?;
    follow_up.wait(context()).await?;
    let inbox = harness
        .session()
        .snapshot(
            &rpi_durable::harness::inbox::INBOX_DOC,
            DocOwner::Conversation(conversation.id()),
            context(),
        )
        .await?
        .and_then(|object| object.get("items").cloned())
        .unwrap_or(JsonValue::Null);
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
            context(),
        )
        .await?
        .items;
    let requests = provider
        .requests
        .lock()
        .map_err(|_| "provider request capture poisoned")?
        .clone();
    let trace = json!({"name":"busy-steer-followup-reject-withdraw","reject":reject,"queuedStatuses":queued_statuses,
        "withdrawal":withdrawal,"inboxWhileBusy":inbox_while_busy,"inbox":inbox,"entries":entries,"requests":requests});
    harness.close(context()).await?;
    Ok(trace)
}

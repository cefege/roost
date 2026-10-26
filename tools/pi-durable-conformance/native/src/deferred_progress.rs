//! Deferred live-document persistence oracle scenario and stream provider.
//! Called by `recovery_traces` with SQLite storage and durable progress APIs.
//! Observes in-memory progress before deferred persistence is flushed.
use super::*;
use crate::provider::FauxUsageEstimator;
struct StreamAuth;

#[async_trait]
impl ApiKeyAuth for StreamAuth {
    fn name(&self) -> &str {
        "pi-durable-conformance-stream"
    }

    async fn resolve(
        &self,
        _context: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, rpi_ai::auth::ModelsError> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: None,
                base_url: None,
            },
            env: None,
            source: None,
        }))
    }
}

struct HeldPartialProvider {
    model: Model,
    auth: ProviderAuth,
    release: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    usage: FauxUsageEstimator,
}

impl HeldPartialProvider {
    fn new(release: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>) -> (Models, Arc<Self>) {
        let provider = Arc::new(Self {
            model: Model {
                id: "faux-1".to_string(),
                name: "Faux 1".to_string(),
                api: ApiKind::from("faux"),
                provider: "faux".to_string(),
                base_url: "http://localhost".to_string(),
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
            auth: ProviderAuth {
                api_key: Some(Arc::new(StreamAuth)),
                oauth: None,
            },
            release,
            usage: FauxUsageEstimator::default(),
        });
        let models = Models::new(None);
        models.set_provider(Arc::clone(&provider) as Arc<dyn Provider>);
        (models, provider)
    }

    fn stream_message(
        &self,
        context: &TranscriptContext,
        options: Option<&StreamOptions>,
    ) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let text = "partial-progress ".repeat(6);
        let mut final_message = message(&text, StopReason::Stop, None);
        final_message.content = vec![AssistantContent::Text(TextContent {
            text: text.clone(),
            text_signature: None,
        })];
        self.usage.apply(&mut final_message, context, options);
        let mut partial = final_message.clone();
        partial.content.clear();
        partial.stop_reason = StopReason::Pending;
        stream.push(StreamEvent::Start {
            partial: Arc::new(partial.clone()),
        });
        partial.content = vec![AssistantContent::Text(TextContent {
            text: String::new(),
            text_signature: None,
        })];
        stream.push(StreamEvent::TextStart {
            content_index: 0,
            partial: Arc::new(partial.clone()),
        });
        let mut release = match self.release.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let result_stream = stream.clone();
        tokio::spawn(async move {
            let boundaries = text
                .char_indices()
                .map(|(index, _)| index)
                .step_by(16)
                .collect::<Vec<_>>();
            let mut progress = String::new();
            for (chunk_index, start) in boundaries.iter().enumerate() {
                let end = boundaries
                    .get(chunk_index + 1)
                    .copied()
                    .unwrap_or(text.len());
                let delta = &text[*start..end];
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                progress.push_str(delta);
                partial.content = vec![AssistantContent::Text(TextContent {
                    text: progress.clone(),
                    text_signature: None,
                })];
                stream.push(StreamEvent::TextDelta {
                    content_index: 0,
                    delta: delta.to_string(),
                    partial: Arc::new(partial.clone()),
                });
                if chunk_index == 1 {
                    if let Some(receiver) = release.take() {
                        let _ = receiver.await;
                    }
                }
            }
            stream.push(StreamEvent::TextEnd {
                content_index: 0,
                content: text.clone(),
                partial: Arc::new(partial),
            });
            stream.push(StreamEvent::Done {
                reason: DoneReason::Stop,
                message: final_message,
            });
        });
        result_stream
    }
}

impl Provider for HeldPartialProvider {
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
    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }
    fn get_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }
    fn stream(
        &self,
        _model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_message(context, options.as_ref())
    }
    fn stream_simple(
        &self,
        _model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        Ok(self.stream_message(context, options.as_ref().map(|options| &options.stream)))
    }
}

pub async fn deferred_document_progress() -> Result<Value, Box<dyn Error>> {
    let scratch = ScratchDatabase(scratch_sqlite_path()?);
    let path = scratch.0.as_str();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let release = Arc::new(Mutex::new(Some(release_receiver)));
    let (models, _provider) = HeldPartialProvider::new(release);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    let storage: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(&path, SqliteDatabaseOptions::default()).await?);
    let mut harness_options: HarnessOptions = options(models, Arc::clone(&registry));
    harness_options.settings = Some(rpi_durable::harness::HarnessSettings {
        progress: Some(rpi_durable::harness::types::ProgressPolicyPatch {
            persist_within_ms: Some(1_200.0),
            ..Default::default()
        }),
        ..Default::default()
    });
    let harness = open_harness(Arc::clone(&storage), harness_options, &BACKGROUND_CONTEXT).await?;
    let root = harness
        .root(
            &BACKGROUND_CONTEXT,
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
    let reader: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(&path, SqliteDatabaseOptions::default()).await?);
    let submission = root
        .submit(input("stream progress", None), &BACKGROUND_CONTEXT)
        .await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let memory_live = loop {
        if let Some(value) = harness
            .session()
            .snapshot(
                &LIVE_DOC,
                DocOwner::Conversation(root.id()),
                &BACKGROUND_CONTEXT,
            )
            .await?
        {
            if value
                .get("generation")
                .and_then(Value::as_object)
                .and_then(|generation| generation.get("message"))
                .and_then(Value::as_object)
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .is_some_and(|content| {
                    content.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("text")
                            && item
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|text| text.len() > 20)
                    })
                })
            {
                break Value::Object(value);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = release_sender.send(());
            let _ = submission.wait(&BACKGROUND_CONTEXT).await;
            let _ = harness.close(&BACKGROUND_CONTEXT).await;
            let _ = storage.close(&BACKGROUND_CONTEXT).await;
            let _ = reader.close(&BACKGROUND_CONTEXT).await;
            return Err("streaming provider did not publish a live partial".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    let address = DocumentAddress {
        kind: LIVE_DOC.definition.kind.clone(),
        scope: DocumentScope::Conversation {
            conversation_id: root.id(),
        },
        key: None,
    };
    let during_record = reader
        .find_document(address.clone(), DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
        .await?;
    let stored_during = if let Some(record) = during_record {
        reader
            .document(record.id, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
            .await?
            .map(|document| Value::Object(document.value))
    } else {
        None
    };
    reader.close(&BACKGROUND_CONTEXT).await?;
    release_sender
        .send(())
        .map_err(|_| "streaming provider ended before release")?;
    submission.wait(&BACKGROUND_CONTEXT).await?;
    let reopened_reader: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(path, SqliteDatabaseOptions::default()).await?);
    let after_record = reopened_reader
        .find_document(address, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
        .await?;
    let stored_after = if let Some(record) = after_record {
        reopened_reader
            .document(record.id, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
            .await?
            .map(serde_json::to_value)
            .transpose()?
    } else {
        None
    };
    let root_entries = entries(root.as_ref()).await?;
    let trace = json!({
        "name": "deferred-document-progress",
        "memoryLive": memory_live,
        "storedDuringProgress": stored_during,
        "storedAfterCompletion": stored_after,
        "entries": serde_json::to_value(root_entries)?,
    });
    if stored_during.as_ref().is_some_and(|stored| {
        stored
            .get("generation")
            .and_then(Value::as_object)
            .and_then(|generation| generation.get("message"))
            .is_some()
    }) {
        return Err("live progress was flushed before its deferred persistence window".into());
    }
    harness.close(&BACKGROUND_CONTEXT).await?;
    storage.close(&BACKGROUND_CONTEXT).await?;
    reopened_reader.close(&BACKGROUND_CONTEXT).await?;
    drop(scratch);
    Ok(trace)
}

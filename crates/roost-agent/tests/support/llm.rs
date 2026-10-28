//! The scripted model of the harness tests: replies queued per session id
//! (with a shared fallback queue), judgments queued in order, and every
//! request, judgment, rotation and web search recorded for assertions.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use roost_agent::{AccountUsage, AgentRuntime, FetchedPage, Llm, NewConversation};
use roost_llm::web_search::{ParsedQuery, SearchQuery, SearchResponse};
use roost_llm::{
    Answer, Catalog, ChatRequest, LlmError, ModelInfo, Question, ResolvedAuth, StreamEvent,
    UserContent,
};
use tokio_util::sync::CancellationToken;

pub type Script = Result<Vec<StreamEvent>, LlmError>;
pub type JudgeScript = Result<BTreeMap<String, Answer>, LlmError>;
pub type SearchScript = Result<SearchResponse, LlmError>;
pub type PartialScript = (Vec<StreamEvent>, LlmError);
/// Entered/released gates for one session's Nth stream, keyed by session id and call index.
pub type StreamGates = Mutex<HashMap<(String, usize), (usize, Arc<ModelStreamGate>)>>;

/// Replies for any session whose own queue is empty (children with fresh ids).
pub const ANY_SESSION: &str = "*";

#[derive(Default)]
pub struct ModelStreamGate {
    pub entered: tokio::sync::Notify,
    pub released: tokio::sync::Notify,
}

/// A model whose replies are queued per `ChatRequest::session_id`.
#[derive(Default)]
pub struct ScriptedLlm {
    pub catalog: Option<Catalog>,
    pub providers: Mutex<Vec<String>>,
    pub scripts: Mutex<HashMap<String, VecDeque<Script>>>,
    pub stream_gates: StreamGates,
    pub credential_gate: Mutex<Option<Arc<ModelStreamGate>>>,
    pub partials: Mutex<HashMap<String, VecDeque<PartialScript>>>,
    pub judgments: Mutex<VecDeque<JudgeScript>>,
    pub requests: Mutex<Vec<ChatRequest>>,
    pub judge_requests: Mutex<Vec<(String, serde_json::Value, Vec<Question>)>>,
    pub rotations: Mutex<Vec<i64>>,
    pub usage_rows: Mutex<Vec<AccountUsage>>,
    pub pages: Mutex<HashMap<String, FetchedPage>>,
    pub engine_results: Mutex<HashMap<String, VecDeque<SearchScript>>>,
    pub hosted_results: Mutex<VecDeque<SearchScript>>,
    pub search_calls: Mutex<Vec<String>>,
    pub search_base_urls: Mutex<Vec<Option<String>>>,
    pub hosted_models: Mutex<Vec<String>>,
}

impl ScriptedLlm {
    pub fn new(providers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            catalog: Some(Catalog::builtin()),
            providers: Mutex::new(
                providers
                    .iter()
                    .map(|provider| (*provider).to_owned())
                    .collect(),
            ),
            ..Self::default()
        })
    }

    pub fn script(&self, session_id: &str, reply: Script) {
        self.scripts
            .lock()
            .unwrap()
            .entry(session_id.to_owned())
            .or_default()
            .push_back(reply);
    }

    pub fn hold_stream(&self, session_id: &str, request_number: usize) -> Arc<ModelStreamGate> {
        self.hold_stream_after_events(session_id, request_number, 0)
    }

    pub fn hold_stream_after_events(
        &self,
        session_id: &str,
        request_number: usize,
        event_count: usize,
    ) -> Arc<ModelStreamGate> {
        let gate = Arc::new(ModelStreamGate::default());
        self.stream_gates.lock().unwrap().insert(
            (session_id.to_owned(), request_number),
            (event_count, gate.clone()),
        );
        gate
    }

    pub fn hold_credential_check(&self) -> Arc<ModelStreamGate> {
        let gate = Arc::new(ModelStreamGate::default());
        *self.credential_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// A reply that streams `events` and then fails, so the response has
    /// visible output by the time the error arrives.
    pub fn partial_then_fail(&self, session_id: &str, events: Vec<StreamEvent>, error: LlmError) {
        self.partials
            .lock()
            .unwrap()
            .entry(session_id.to_owned())
            .or_default()
            .push_back((events, error));
    }

    pub fn judgment(&self, reply: JudgeScript) {
        self.judgments.lock().unwrap().push_back(reply);
    }

    pub fn requests_for(&self, session_id: &str) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.session_id == session_id)
            .cloned()
            .collect()
    }

    pub fn page(&self, url: &str, page: FetchedPage) {
        self.pages.lock().unwrap().insert(url.to_owned(), page);
    }

    pub fn engine(&self, engine_id: &str, reply: SearchScript) {
        self.engine_results
            .lock()
            .unwrap()
            .entry(engine_id.to_owned())
            .or_default()
            .push_back(reply);
    }

    pub fn hosted(&self, reply: SearchScript) {
        self.hosted_results.lock().unwrap().push_back(reply);
    }
}

pub fn user_content(request: &ChatRequest) -> &[UserContent] {
    request
        .messages
        .iter()
        .find_map(|message| match message {
            roost_llm::Message::User { content } => Some(content.as_slice()),
            _ => None,
        })
        .expect("a model request includes user content")
}

pub async fn unpinned_conversation(runtime: &AgentRuntime) -> String {
    runtime
        .create_conversation(NewConversation {
            worker_fp: "worker-fp".into(),
            worker_label: "desktop".into(),
            worker_os: "linux".into(),
            cwd: "/repo".into(),
            title: None,
            model: None,
            thinking_level: None,
        })
        .await
        .expect("the test conversation can be created")
        .id
}

impl Llm for ScriptedLlm {
    fn catalog(&self) -> &Catalog {
        self.catalog.as_ref().unwrap()
    }

    fn has_credential<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, bool> {
        let known = self
            .providers
            .lock()
            .unwrap()
            .iter()
            .any(|known| known == provider);
        let gate = self.credential_gate.lock().unwrap().take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.released.notified().await;
            }
            known
        })
    }

    fn resolve<'a>(
        &'a self,
        provider: &'a str,
        _conversation_id: &'a str,
    ) -> BoxFuture<'a, Result<(i64, ResolvedAuth), LlmError>> {
        let known = self
            .providers
            .lock()
            .unwrap()
            .iter()
            .any(|known| known == provider);
        Box::pin(async move {
            if known {
                Ok((
                    1,
                    ResolvedAuth::ApiKey {
                        key: "first".into(),
                    },
                ))
            } else {
                Err(LlmError::NoCredential {
                    provider: provider.to_owned(),
                })
            }
        })
    }

    fn rotate<'a>(
        &'a self,
        credential_id: i64,
        _provider: &'a str,
        _conversation_id: &'a str,
        _error: &'a LlmError,
    ) -> BoxFuture<'a, Result<(i64, ResolvedAuth), LlmError>> {
        self.rotations.lock().unwrap().push(credential_id);
        Box::pin(async move {
            Ok((
                credential_id + 1,
                ResolvedAuth::ApiKey { key: "next".into() },
            ))
        })
    }

    fn stream(
        &self,
        _credential_id: i64,
        request: ChatRequest,
        _auth: ResolvedAuth,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, LlmError>> {
        let session = request.session_id.clone();
        let request_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests
                .iter()
                .filter(|request| request.session_id == session)
                .count()
        };
        let partial = self
            .partials
            .lock()
            .unwrap()
            .get_mut(&session)
            .and_then(VecDeque::pop_front);
        let events = if let Some((events, error)) = partial {
            let mut items: Vec<Result<StreamEvent, LlmError>> =
                events.into_iter().map(Ok).collect();
            items.push(Err(error));
            items
        } else {
            let next = {
                let mut scripts = self.scripts.lock().unwrap();
                let own = scripts.get_mut(&session).and_then(VecDeque::pop_front);
                own.or_else(|| scripts.get_mut(ANY_SESSION).and_then(VecDeque::pop_front))
            };
            match next {
                Some(Ok(events)) => events.into_iter().map(Ok).collect(),
                Some(Err(error)) => vec![Err(error)],
                None => vec![Err(LlmError::Decode(format!(
                    "no script left for {session}"
                )))],
            }
        };
        let gate = self
            .stream_gates
            .lock()
            .unwrap()
            .remove(&(session, request_number));
        Box::pin(futures::stream::unfold(
            (gate, events.into_iter(), 0_usize),
            move |(gate, mut events, mut emitted)| {
                let cancel = cancel.clone();
                async move {
                    let gate = match gate {
                        Some((after_events, gate)) if emitted == after_events => {
                            gate.entered.notify_one();
                            tokio::select! {
                                () = gate.released.notified() => {}
                                () = cancel.cancelled() => return None,
                            }
                            None
                        }
                        retained => retained,
                    };
                    let event = events.next()?;
                    emitted += 1;
                    Some((event, (gate, events, emitted)))
                }
            },
        ))
    }

    fn judge<'a>(
        &'a self,
        model: &'a ModelInfo,
        state: serde_json::Value,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Answer>, LlmError>> {
        self.judge_requests.lock().unwrap().push((
            format!("{}/{}", model.provider, model.id),
            state,
            questions,
        ));
        let next = self.judgments.lock().unwrap().pop_front();
        Box::pin(async move {
            next.unwrap_or_else(|| Err(LlmError::Decode("no judgment scripted".into())))
        })
    }

    fn account_usage(&self) -> BoxFuture<'_, Vec<AccountUsage>> {
        let rows = self.usage_rows.lock().unwrap().clone();
        Box::pin(async move { rows })
    }

    fn fetch_url<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<FetchedPage, LlmError>> {
        let page = self.pages.lock().unwrap().get(url).cloned();
        Box::pin(async move {
            page.ok_or_else(|| LlmError::Decode(format!("no page scripted for {url}")))
        })
    }

    fn search_engine<'a>(
        &'a self,
        engine_id: &'a str,
        base_url: Option<&'a str>,
        _query: &'a SearchQuery,
        _parsed: &'a ParsedQuery,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchResponse, LlmError>> {
        self.search_calls.lock().unwrap().push(engine_id.to_owned());
        self.search_base_urls
            .lock()
            .unwrap()
            .push(base_url.map(str::to_owned));
        let next = self
            .engine_results
            .lock()
            .unwrap()
            .get_mut(engine_id)
            .and_then(VecDeque::pop_front);
        Box::pin(async move {
            next.unwrap_or_else(|| {
                Err(LlmError::Decode(format!(
                    "no search scripted for {engine_id}"
                )))
            })
        })
    }

    fn engine_configured<'a>(&'a self, engine_id: &'a str) -> BoxFuture<'a, bool> {
        let configured = self
            .providers
            .lock()
            .unwrap()
            .iter()
            .any(|provider| provider == engine_id);
        Box::pin(async move { configured })
    }

    fn hosted_search<'a>(
        &'a self,
        model: &'a ModelInfo,
        _conversation_id: &'a str,
        _query: &'a SearchQuery,
        _parsed: &'a ParsedQuery,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<SearchResponse, LlmError>> {
        self.hosted_models
            .lock()
            .unwrap()
            .push(format!("{}/{}", model.provider, model.id));
        self.search_calls.lock().unwrap().push("hosted".into());
        let next = self.hosted_results.lock().unwrap().pop_front();
        Box::pin(async move {
            next.unwrap_or_else(|| Err(LlmError::Decode("no hosted search scripted".into())))
        })
    }
}

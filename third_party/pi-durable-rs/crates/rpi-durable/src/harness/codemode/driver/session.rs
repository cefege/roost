//! Replay admission, result delivery, and journal commit state for one run.
//!
//! The worker supplies deterministic call IDs; this state machine persists
//! result order and issued-count barriers before launching later effects.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use futures::FutureExt;
use futures::stream::FuturesUnordered;
use rpi_codemode::portable::{CodemodeRun, Outcome};
use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use crate::Context;
use crate::harness::codemode::driver::calls::{CallCompletion, HostCall};
use crate::harness::codemode::driver::{self, CallFuture};
use crate::harness::codemode::models;
use crate::harness::codemode::state::{JournalEntry, JournalState, RunState};
use crate::harness::nested::{NESTED_CALL_DOC, NestedCallState};
use crate::harness::types::{ToolExecutionApi, ToolExecutionResult};
use crate::ids::{ConversationId, TaskId};
use crate::session::DocOwner;

pub(super) struct Driver {
    pub(super) api: Arc<dyn ToolExecutionApi>,
    pub(super) context: Context,
    task_id: TaskId,
    conversation_id: ConversationId,
    pub(super) run_state: Arc<RunState>,
    journal: JournalState,
    replayed: Vec<JournalEntry>,
    replay_index: HashMap<u32, usize>,
    next_replayed: usize,
    replaying: bool,
    rerun: bool,
    released: bool,
    durable_issued: u32,
    issued: u32,
    waiting: Vec<HostCall>,
    held: Vec<HostCall>,
    ready: HashMap<u32, Outcome>,
    queue: VecDeque<CallCompletion>,
    acknowledgements: BTreeMap<u64, u32>,
    journal_length: usize,
    pub(super) generated_images: usize,
    model_slots: Arc<Semaphore>,
}

impl Driver {
    pub(super) fn new(
        run_state: RunState,
        journal: JournalState,
        rerun: bool,
        api: Arc<dyn ToolExecutionApi>,
        context: Context,
    ) -> Self {
        let replay_index = journal
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        let durable_issued = journal
            .entries
            .iter()
            .filter_map(|entry| entry.issued_after)
            .chain(journal.first_issued)
            .max()
            .unwrap_or(0);
        let journal_length = journal.entries.len();
        let replaying = journal_length != 0;
        let task_id = api.task_id();
        let conversation_id = api.conversation_id();
        Self {
            api,
            context,
            task_id,
            conversation_id,
            run_state: Arc::new(run_state),
            replayed: journal.entries.clone(),
            replay_index,
            next_replayed: 0,
            replaying,
            rerun,
            released: false,
            durable_issued,
            issued: 0,
            waiting: Vec::new(),
            held: Vec::new(),
            ready: HashMap::new(),
            queue: VecDeque::new(),
            acknowledgements: BTreeMap::new(),
            journal,
            journal_length,
            generated_images: 0,
            model_slots: Arc::new(Semaphore::new(driver::MAX_CONCURRENT_MODEL_CALLS)),
        }
    }

    pub(super) async fn on_call(
        &mut self,
        call: HostCall,
        run: &mut CodemodeRun,
        running: &mut FuturesUnordered<CallFuture>,
    ) -> Result<(), String> {
        self.issued = self.issued.max(call.id);
        if let Some(index) = self.replay_index.get(&call.id).copied() {
            let entry = &self.replayed[index];
            if let Some(global) = &entry.global {
                if call.global.as_deref() != Some(global.name.as_str())
                    || call.name != global.name
                    || call.args != global.args
                {
                    return Err(format!(
                        "call {} changed while replaying its durable global result",
                        call.id
                    ));
                }
                self.ready
                    .insert(call.id, driver::calls::outcome_from_json(&global.outcome)?);
                self.deliver_replayed(run)?;
                return Ok(());
            }
            if call.global.is_some() {
                return Err(format!(
                    "call {} changed from a tool call to a global call",
                    call.id
                ));
            }
            self.launch(call, running);
            return Ok(());
        }
        if self.released {
            self.waiting.push(call);
            self.launch_covered(running);
        } else {
            self.held.push(call);
        }
        Ok(())
    }

    pub(super) async fn on_idle(
        &mut self,
        seq: u64,
        issued: u32,
        run: &mut CodemodeRun,
        running: &mut FuturesUnordered<CallFuture>,
    ) -> Result<(), String> {
        if self.rerun && seq <= self.replayed.len() as u64 {
            let recorded = if seq == 0 {
                self.journal.first_issued
            } else {
                self.replayed
                    .get(usize::try_from(seq - 1).map_err(|error| error.to_string())?)
                    .and_then(|entry| entry.issued_after)
            };
            if recorded.is_some_and(|recorded| recorded != issued) {
                let recorded = recorded.unwrap_or_default();
                return Err(if seq == 0 {
                    format!("the first run made {issued} calls, where it had made {recorded}")
                } else {
                    format!(
                        "after result {seq} it had made {issued} calls, where it had made {recorded}"
                    )
                });
            }
        }
        self.acknowledgements.insert(seq, issued);
        if self.replaying {
            if seq != self.next_replayed as u64 {
                return Ok(());
            }
            if let Some(next) = self.replayed.get(self.next_replayed) {
                if next.id > issued {
                    return Err(format!(
                        "the script waits without making call {}, which it made before",
                        next.id
                    ));
                }
                return Ok(());
            }
            self.replaying = false;
            self.pump(run, running).await?;
            self.release(running).await?;
            return Ok(());
        }
        if usize::try_from(seq).ok() == Some(self.journal_length) {
            self.pump(run, running).await?;
        }
        if seq == 0 && !self.released {
            self.release(running).await?;
        }
        Ok(())
    }

    pub(super) async fn on_completion(
        &mut self,
        completion: CallCompletion,
        run: &mut CodemodeRun,
        running: &mut FuturesUnordered<CallFuture>,
    ) -> Result<(), String> {
        self.generated_images = self
            .generated_images
            .saturating_add(completion.generated_images);
        if self.replay_index.contains_key(&completion.entry.id) {
            self.ready.insert(completion.entry.id, completion.outcome);
            return self.deliver_replayed(run);
        }
        self.queue.push_back(completion);
        self.pump(run, running).await
    }

    pub(super) fn finish_divergence(
        &self,
        during: rpi_codemode::portable::During,
    ) -> Option<String> {
        if !self.rerun || during.seq > self.replayed.len() as u64 {
            return None;
        }
        let continued = if during.seq == 0 {
            !self.replayed.is_empty() || self.journal.first_issued.is_some()
        } else {
            self.replayed
                .get(usize::try_from(during.seq - 1).ok()?)
                .is_some_and(|entry| entry.issued_after.is_some())
        } || self
            .replayed
            .iter()
            .skip(usize::try_from(during.seq).ok()?)
            .any(|entry| entry.id > during.issued);
        continued.then(|| {
            format!(
                "the script ended after result {}, where it went on before",
                during.seq
            )
        })
    }

    pub(super) async fn persist_failure(
        &self,
        message: &str,
    ) -> Result<ToolExecutionResult, crate::harness::types::ToolExecuteError> {
        let message = if self.rerun {
            format!("The script took a different path when resumed after a restart: {message}")
        } else {
            message.to_string()
        };
        driver::persist_error(
            Arc::clone(&self.api),
            self.task_id,
            self.conversation_id,
            &self.context,
            &message,
        )
        .await
    }

    fn launch(&self, call: HostCall, running: &mut FuturesUnordered<CallFuture>) {
        let api = Arc::clone(&self.api);
        let context = self.context.clone();
        let run_state = Arc::clone(&self.run_state);
        let model_slots = Arc::clone(&self.model_slots);
        running.push(
            async move {
                driver::calls::execute(call, &run_state.tools, api, context, model_slots).await
            }
            .boxed(),
        );
    }

    fn launch_covered(&mut self, running: &mut FuturesUnordered<CallFuture>) {
        let waiting = std::mem::take(&mut self.waiting);
        let mut pending = Vec::new();
        for call in waiting {
            if call.id <= self.durable_issued {
                self.launch(call, running);
            } else {
                pending.push(call);
            }
        }
        self.waiting = pending;
    }

    fn deliver_replayed(&mut self, run: &mut CodemodeRun) -> Result<(), String> {
        while let Some(entry) = self.replayed.get(self.next_replayed) {
            if entry.id > self.issued {
                break;
            }
            let Some(outcome) = self.ready.remove(&entry.id) else {
                break;
            };
            self.next_replayed += 1;
            run.deliver(self.next_replayed as u64, entry.at, entry.id, outcome)
                .map_err(|error| format!("could not deliver replayed code-mode result: {error}"))?;
        }
        Ok(())
    }

    async fn release(&mut self, running: &mut FuturesUnordered<CallFuture>) -> Result<(), String> {
        if self.released {
            return Ok(());
        }
        let held = std::mem::take(&mut self.held);
        if self.rerun {
            for call in &held {
                if let Some(other) = self.earlier(call).await? {
                    return Err(format!(
                        "call {} is {}, but was {}",
                        call.id,
                        driver::describe(call),
                        other
                    ));
                }
            }
        }
        self.released = true;
        self.waiting.extend(held);
        self.launch_covered(running);
        Ok(())
    }

    async fn earlier(&self, call: &HostCall) -> Result<Option<String>, String> {
        let key = format!("c{}", call.id);
        let snapshot = self
            .api
            .snapshot(
                &NESTED_CALL_DOC,
                DocOwner::TaskFamily(self.task_id, &key),
                &self.context,
            )
            .await
            .map_err(|error| error.to_string())?;
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let state: NestedCallState =
            serde_json::from_value(Value::Object(snapshot)).map_err(|error| error.to_string())?;
        if state.task_id.is_none() {
            return Ok(None);
        }
        if let Some((expected_name, expected_args)) = Self::expected_nested_identity(call) {
            if state.name.as_deref() == Some(expected_name.as_str())
                && state.arguments.as_ref() == Some(&expected_args)
            {
                return Ok(None);
            }
        }
        Ok(Some(format!(
            "tools.{}({})",
            state.name.unwrap_or_default(),
            state.arguments.map(Value::Object).unwrap_or(Value::Null)
        )))
    }

    fn expected_nested_identity(call: &HostCall) -> Option<(String, Map<String, Value>)> {
        if call.global.is_none() {
            let args = call
                .args
                .as_ref()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            return Some((call.name.clone(), args));
        }
        let arguments = call.args.as_ref()?.as_array()?;
        if arguments.len() != 2 {
            return None;
        }
        let tool_name = match call.name.as_str() {
            "models.classify" => models::CLASSIFY_TOOL,
            "models.generateImages" => models::GENERATE_IMAGES_TOOL,
            _ => return None,
        };
        Some((
            tool_name.to_string(),
            serde_json::json!({ "model": arguments[0], "context": arguments[1] })
                .as_object()?
                .clone(),
        ))
    }

    async fn pump(
        &mut self,
        run: &mut CodemodeRun,
        running: &mut FuturesUnordered<CallFuture>,
    ) -> Result<(), String> {
        if self.queue.is_empty() && self.acknowledgements.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.queue)
            .into_iter()
            .collect::<Vec<_>>();
        let acknowledgements = std::mem::take(&mut self.acknowledgements)
            .into_iter()
            .collect::<Vec<_>>();
        let entries = batch
            .iter()
            .map(|completion| completion.entry.clone())
            .collect();
        crate::harness::codemode::state::persist_journal_batch(
            self.api.as_ref(),
            self.task_id,
            entries,
            acknowledgements.clone(),
            &self.context,
        )
        .await
        .map_err(|error| error.to_string())?;
        for (seq, issued) in acknowledgements {
            self.durable_issued = self.durable_issued.max(issued);
            if seq == 0 {
                self.journal.first_issued.get_or_insert(issued);
            } else if let Some(entry) = usize::try_from(seq - 1)
                .ok()
                .and_then(|index| self.journal.entries.get_mut(index))
            {
                entry.issued_after.get_or_insert(issued);
            }
        }
        for completion in batch {
            let entry = completion.entry;
            self.journal.entries.push(entry.clone());
            self.journal_length += 1;
            run.deliver(
                self.journal_length as u64,
                entry.at,
                entry.id,
                completion.outcome,
            )
            .map_err(|error| format!("could not deliver code-mode result: {error}"))?;
        }
        self.launch_covered(running);
        Ok(())
    }
}

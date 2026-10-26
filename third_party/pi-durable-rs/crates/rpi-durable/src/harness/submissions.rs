//! Durable submission admission, waits, and withdrawal (port of
//! `packages/durable/src/harness/submissions.ts` @ `abe508e1b`).
//!
//! A submission is the durable hand-off between a host and a conversation: an
//! *input* may start a run, a *write* appends a passive entry. Admission is one
//! commit: a known request ID returns its existing submission without writing, a
//! busy conversation queues the submission in `pi.inbox` (or rejects
//! `whenBusy: "reject"` input with `ConversationBusy`), and an idle conversation
//! places it directly. Waits register on the session line so no settling
//! publication can fall between the check and the registration.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use crate::chord::context::Context;
use crate::errors::StorageError;
use crate::harness::inbox::{
    INBOX_DOC, InboxItem, InboxState, QueueModes, apply_boundary, entry_to_json, is_stale,
    prepare_boundary, remove_inbox_item,
};
use crate::harness::live::LIVE_DOC;
use crate::harness::scheduler::SchedulerService;
use crate::harness::types::{
    InputSubmissionDraft, Submission, SubmissionAbort, SubmissionAbortResult, SubmissionDraft,
    WhenBusy, read_state, write_state,
};
use crate::harness::util::{WaitError, WaitFuture, Waiters, closed_error};
use crate::ids::{ConversationId, SubmissionId};
use crate::session::{
    DocOwner, NowFn, SessionError, SessionImpl, SessionResult, SubmissionSettlement, commit_fn,
};
use crate::types::SubmissionRecord;

/// Admission, waits, and withdrawal of the durable submissions of one Harness.
pub struct Submissions {
    session: Arc<SessionImpl>,
    scheduler: Arc<dyn SchedulerService>,
    now: NowFn,
    queue_modes: Box<dyn Fn() -> QueueModes + Send + Sync>,
    resume: Arc<dyn Fn() -> SessionResult<()> + Send + Sync>,
    waiters: Waiters<SubmissionId, SubmissionRecord>,
    closed: Arc<AtomicBool>,
}

impl Submissions {
    /// Build the service and subscribe it to the session's publications and
    /// close. The `resume` callback enables scheduling; submitting or waiting
    /// asks for progress.
    pub fn open(
        session: Arc<SessionImpl>,
        scheduler: Arc<dyn SchedulerService>,
        now: NowFn,
        queue_modes: Box<dyn Fn() -> QueueModes + Send + Sync>,
        resume: Arc<dyn Fn() -> SessionResult<()> + Send + Sync>,
    ) -> Arc<Self> {
        let submissions = Arc::new(Self {
            session,
            scheduler,
            now,
            queue_modes,
            resume,
            waiters: Waiters::new(),
            closed: Arc::new(AtomicBool::new(false)),
        });
        let observe = Arc::clone(&submissions);
        submissions
            .session
            .subscribe_commits(Arc::new(move |publication, _context| {
                observe.observe(publication);
            }));
        let closing = Arc::clone(&submissions);
        submissions.session.subscribe_close(Arc::new(move || {
            closing.closed.store(true, Ordering::SeqCst);
            closing.waiters.reject_all(closed_error());
        }));
        submissions
    }

    /// Admit a submission in one commit; see [`admit_submission`].
    pub async fn submit(
        self: &Arc<Self>,
        conversation_id: ConversationId,
        draft: SubmissionDraft,
        context: &Context,
    ) -> SessionResult<Arc<dyn Submission>> {
        (self.resume)()?;
        let scheduler = Arc::clone(&self.scheduler);
        let now = (self.now)();
        let modes = (self.queue_modes)();
        let id = self
            .session
            .commit_with(
                commit_fn(move |tx| {
                    let scheduler = Arc::clone(&scheduler);
                    let draft = draft.clone();
                    Box::pin(async move {
                        admit_submission(tx, conversation_id, &draft, now, modes, &*scheduler).await
                    })
                }),
                context,
                None,
            )
            .await?;
        Ok(Arc::new(SubmissionHandle {
            id,
            submissions: Arc::clone(self),
        }))
    }

    /// Handle for an existing submission, or `None`.
    pub async fn get(
        self: &Arc<Self>,
        id: SubmissionId,
        context: &Context,
    ) -> SessionResult<Option<Arc<dyn Submission>>> {
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        let record = session
            .read_on_line(move || {
                Box::pin(
                    async move { Ok(job_session.storage().submission(id, &read_context).await?) },
                )
            })
            .await?;
        Ok(record.map(|record| {
            Arc::new(SubmissionHandle {
                id: record.id(),
                submissions: Arc::clone(self),
            }) as Arc<dyn Submission>
        }))
    }

    /// The committed record, or an error when it does not exist.
    pub async fn status(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> SessionResult<SubmissionRecord> {
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let read_context = context.clone();
        let record = session
            .read_on_line(move || {
                Box::pin(
                    async move { Ok(job_session.storage().submission(id, &read_context).await?) },
                )
            })
            .await?;
        record.ok_or_else(|| SessionError::message(format!("Submission {id} does not exist")))
    }

    /// Resolve when the submission reaches a terminal status.
    pub async fn wait(
        &self,
        id: SubmissionId,
        context: &Context,
    ) -> SessionResult<SubmissionRecord> {
        (self.resume)()?;
        // Check and register on the line so no settling publication falls between them.
        // FORM: upstream returns a promise; the port returns a `WaitFuture` from the
        // line job and awaits it after the line releases.
        let session = Arc::clone(&self.session);
        let job_session = Arc::clone(&session);
        let waiters = self.waiters.clone();
        let closed = Arc::clone(&self.closed);
        let read_context = context.clone();
        let found: SessionResult<WaitFuture<SubmissionRecord>> = session
            .read_on_line(move || {
                let waiters = waiters.clone();
                let closed = Arc::clone(&closed);
                let read_context = read_context.clone();
                let job_session = Arc::clone(&job_session);
                Box::pin(async move {
                    let record = job_session.storage().submission(id, &read_context).await?;
                    let Some(record) = record else {
                        return Err(SessionError::message(format!(
                            "Submission {id} does not exist"
                        )));
                    };
                    if is_settled(&record) {
                        return Ok(
                            Box::pin(async move { Ok(record) }) as WaitFuture<SubmissionRecord>
                        );
                    }
                    // Close rejects registered waiters synchronously and may begin during the read.
                    if closed.load(Ordering::SeqCst) {
                        return Err(closed_error().into());
                    }
                    Ok(waiters.add(id, &read_context))
                })
            })
            .await;
        found?.await.map_err(Into::into)
    }

    /// Withdraw a queued submission and remove its inbox item; placed inputs and
    /// settled submissions are reported.
    pub async fn abort(
        &self,
        id: SubmissionId,
        context: &Context,
        conversation_id: Option<ConversationId>,
    ) -> SessionResult<SubmissionAbortResult> {
        self.session
            .commit_with(
                commit_fn(move |tx| {
                    Box::pin(async move {
                        let record = tx.submission(id).await?;
                        let Some(record) = record else {
                            return Ok(SubmissionAbortResult::NotFound);
                        };
                        if conversation_id.is_some_and(|want| want != record.conversation_id()) {
                            return Ok(SubmissionAbortResult::NotFound);
                        }
                        if record.status() == "queued" {
                            tx.settle_submission(
                                id,
                                SubmissionSettlement::Unanswered {
                                    reason: "aborted".to_string(),
                                    detail: None,
                                },
                            )?;
                            remove_inbox_item(tx, record.conversation_id(), id).await?;
                            return Ok(SubmissionAbortResult::Aborted);
                        }
                        Ok(if record.status() == "placed" {
                            SubmissionAbortResult::AlreadyPlaced
                        } else {
                            SubmissionAbortResult::Settled
                        })
                    })
                }),
                context,
                None,
            )
            .await
    }

    fn observe(&self, publication: &crate::session::CommitPublication) {
        for change in &publication.changes {
            if let crate::session::CommitChange::Table(crate::types::StorageWrite::Submission {
                value,
            }) = change
                && is_settled(value)
            {
                self.waiters.resolve(value.id(), value.clone());
            }
        }
    }
}

impl From<WaitError> for SessionError {
    fn from(error: WaitError) -> Self {
        match error {
            WaitError::Aborted => SessionError::Aborted,
            WaitError::Closed => SessionError::Closed,
            WaitError::SessionFailed(cause) => SessionError::SessionFailed(cause),
        }
    }
}

/// A terminal submission (`done` or `unanswered`).
pub fn is_settled(record: &SubmissionRecord) -> bool {
    record.status() == "done" || record.status() == "unanswered"
}

/// Awaitable host object for one durably admitted submission.
pub struct SubmissionHandle {
    id: SubmissionId,
    submissions: Arc<Submissions>,
}

#[async_trait]
impl Submission for SubmissionHandle {
    fn id(&self) -> SubmissionId {
        self.id
    }

    async fn status(&self, context: &Context) -> SessionResult<SubmissionRecord> {
        self.submissions.status(self.id, context).await
    }

    async fn wait(&self, context: &Context) -> SessionResult<SubmissionRecord> {
        self.submissions.wait(self.id, context).await
    }

    async fn abort(&self, context: &Context) -> SessionResult<SubmissionAbort> {
        match self.submissions.abort(self.id, context, None).await? {
            SubmissionAbortResult::Aborted => Ok(SubmissionAbort::Aborted),
            SubmissionAbortResult::AlreadyPlaced => Ok(SubmissionAbort::AlreadyPlaced),
            SubmissionAbortResult::Settled => Ok(SubmissionAbort::Settled),
            SubmissionAbortResult::NotFound => Err(SessionError::message(format!(
                "Submission {} does not exist",
                self.id
            ))),
        }
    }
}

/// Admit a submission inside a commit; `Conversation.submit()` and
/// conversation-owned compactions share it. A known request ID returns its
/// existing submission without writing. A busy conversation queues it in
/// `pi.inbox`, or rejects `whenBusy: "reject"` input with `ConversationBusy`. An
/// idle conversation with queued items queues it behind them and runs a final
/// boundary. Otherwise idle input places a user entry and starts a run, and an
/// idle write appends its entry and settles `done`, or `stale` when its head
/// reaches before the active range.
pub async fn admit_submission(
    tx: &mut crate::session::Transaction,
    conversation_id: ConversationId,
    draft: &SubmissionDraft,
    now: f64,
    queue_modes: QueueModes,
    scheduler: &dyn SchedulerService,
) -> SessionResult<SubmissionId> {
    let (request_id, is_write) = match draft {
        SubmissionDraft::Input(input) => (input.request_id.as_deref(), false),
        SubmissionDraft::Write(write) => (write.request_id.as_deref(), true),
    };
    if let Some(request_id) = request_id
        && let Some(existing) = tx
            .submission_by_request(conversation_id, request_id)
            .await?
    {
        let existing_is_write = matches!(existing, SubmissionRecord::Write { .. });
        if existing_is_write != is_write {
            let existing_type = if existing_is_write { "write" } else { "input" };
            return Err(SessionError::message(format!(
                "Request {request_id} already identifies a submission of type {existing_type}"
            )));
        }
        return Ok(existing.id());
    }
    let live = tx
        .doc(&LIVE_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    let busy = live_is_busy(&live)?;
    if busy
        && let SubmissionDraft::Input(InputSubmissionDraft {
            when_busy: Some(WhenBusy::Reject),
            ..
        }) = draft
    {
        return Err(StorageError::ConversationBusy(conversation_id).into());
    }
    // A boundary reads the table, so it is prepared before the first table write;
    // a busy one needs none.
    let mut boundary = if busy {
        None
    } else {
        Some(prepare_boundary(tx, conversation_id, queue_modes).await?)
    };
    let queued_items = match &boundary {
        Some(boundary) => {
            let state: InboxState = read_state(&boundary.inbox)?;
            !state.items.is_empty()
        }
        None => false,
    };
    if boundary.is_none() || queued_items {
        let create = match draft {
            SubmissionDraft::Input(_) => crate::session::SubmissionCreate::Input {
                conversation_id,
                request_id: request_id.map(str::to_string),
            },
            SubmissionDraft::Write(_) => crate::session::SubmissionCreate::Write {
                conversation_id,
                request_id: request_id.map(str::to_string),
            },
        };
        let record = tx.create_submission(create).await?;
        let id = record.id();
        // Hosts may leave optional fields `undefined`; drafts take strict JSON.
        let item = match draft {
            SubmissionDraft::Write(write) => InboxItem::Write {
                id,
                entry: entry_to_json(&write.entry),
            },
            SubmissionDraft::Input(input) => {
                let mode = match input.when_busy {
                    Some(WhenBusy::Steer) => "steer",
                    _ => "followUp",
                };
                if mode == "steer" {
                    InboxItem::Steer {
                        id,
                        content: input.content.clone(),
                    }
                } else {
                    InboxItem::FollowUp {
                        id,
                        content: input.content.clone(),
                    }
                }
            }
        };
        push_item(tx, conversation_id, boundary.as_ref(), item).await?;
        let Some(boundary) = boundary else {
            return Ok(id);
        };
        let result =
            apply_boundary(tx, boundary, crate::harness::inbox::BoundaryAt::Final, now).await?;
        if !result.users.is_empty() {
            scheduler
                .start_run(tx, conversation_id, &live, result.users)
                .await?;
        }
        return Ok(id);
    }
    // Idle path.
    let boundary = boundary
        .take()
        .ok_or_else(|| SessionError::message("Boundary disappeared after preparation"))?;
    match draft {
        SubmissionDraft::Write(write) => {
            if is_stale(&boundary, &write.entry) {
                let record = tx
                    .create_submission(crate::session::SubmissionCreate::Write {
                        conversation_id,
                        request_id: request_id.map(str::to_string),
                    })
                    .await?;
                tx.settle_submission(
                    record.id(),
                    SubmissionSettlement::Unanswered {
                        reason: "stale".to_string(),
                        detail: None,
                    },
                )?;
                return Ok(record.id());
            }
            let entry = tx
                .append_entry(conversation_id, write.entry.clone())
                .await?;
            let record = tx
                .create_submission(crate::session::SubmissionCreate::Write {
                    conversation_id,
                    request_id: request_id.map(str::to_string),
                })
                .await?;
            tx.place_submission(record.id(), entry.id)?;
            Ok(record.id())
        }
        SubmissionDraft::Input(input) => {
            let entry = append_user_entry(tx, conversation_id, input, now).await?;
            let record = tx
                .create_submission(crate::session::SubmissionCreate::Input {
                    conversation_id,
                    request_id: request_id.map(str::to_string),
                })
                .await?;
            tx.place_submission(record.id(), entry.id)?;
            scheduler
                .start_run(tx, conversation_id, &live, vec![record.id()])
                .await?;
            Ok(record.id())
        }
    }
}

fn live_is_busy(live: &crate::session::DocDraft) -> SessionResult<bool> {
    let state: crate::harness::live::LiveState = read_state(live)?;
    Ok(state.run.is_some())
}

async fn append_user_entry(
    tx: &mut crate::session::Transaction,
    conversation_id: ConversationId,
    input: &InputSubmissionDraft,
    now: f64,
) -> SessionResult<crate::types::EntryRecord> {
    use rpi_ai::types::{Message, UserMessage, UserRole};
    let message = Message::User(UserMessage {
        role: UserRole::User,
        content: input.content.clone(),
        timestamp: now as i64,
    });
    let mut draft = crate::session::EntryDraft::new(crate::entries::USER_ENTRY);
    draft.model = Some(vec![message]);
    tx.append_entry(conversation_id, draft).await
}

async fn push_item(
    tx: &mut crate::session::Transaction,
    conversation_id: ConversationId,
    boundary: Option<&crate::harness::inbox::Boundary>,
    item: InboxItem,
) -> SessionResult<()> {
    let draft = match boundary {
        Some(boundary) => boundary.inbox.clone(),
        None => {
            tx.doc(&INBOX_DOC, DocOwner::Conversation(conversation_id), None)
                .await?
        }
    };
    let mut state: InboxState = read_state(&draft)?;
    state.items.push(item);
    write_state(&draft, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WriteSubmissionStatus;

    #[test]
    fn settled_predicate_matches_terminal_statuses() {
        use crate::ids::id_from_number;
        use crate::types::{InputSubmissionStatus, SubmissionRecord};
        let done = SubmissionRecord::Input {
            id: id_from_number(1),
            conversation_id: id_from_number(1),
            request_id: None,
            status: InputSubmissionStatus::Done {
                entry: id_from_number(1),
                answer: id_from_number(2),
            },
        };
        assert!(is_settled(&done));
        let queued = SubmissionRecord::Write {
            id: id_from_number(2),
            conversation_id: id_from_number(1),
            request_id: None,
            status: WriteSubmissionStatus::Queued,
        };
        assert!(!is_settled(&queued));
    }
}

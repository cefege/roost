//! The per-file upload driver: one chosen file in, one settled path out,
//! queued serially to preserve the user's pick order.
//!
//! Its contract is dedup probe, then direct carriers, then relay. A failure
//! after crossing the byte boundary is ambiguous and is never retried.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use roost_client_core::client::attachments::direct::upload_attachment_direct;
use roost_client_core::client::attachments::insertion::safe_attachment_insertion;

use super::attachment_picker::ChosenFile;
use super::upload_card::{self, UploadPreview};
use super::upload_host::{self, BrowserDirectEnvironment, DirectIdentity};
use super::upload_id::{content_digest, mint_upload_id};
use super::upload_plan::{CarrierChoice, PlanRefusal, UploadOutcome, UploadPlan};
use crate::components::notifications::store_write::write_store;
use crate::pump::Pump;

/// Where a committed path goes once the worker holds it. The composer hands the
/// pane's own input controller in; the context menu hands the same one.
pub type InsertionSink = Rc<dyn Fn(&str)>;

thread_local! {
    /// The uploads waiting for their turn, in the order they were picked.
    ///
    /// Serial because the order IS the payload: a multi-file gesture stores the
    /// files in pick order and types the paths in that order, so two concurrent
    /// uploads would type them in an order nobody chose.
    static PENDING: RefCell<Vec<QueuedUpload>> = const { RefCell::new(Vec::new()) };
    /// Whether a queued upload is running. A flag rather than a promise chain,
    /// because a chain is poisoned by the first rejection, and one failed
    /// upload must not strand every later one.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// One picked file, waiting for its turn.
struct QueuedUpload {
    pump: Pump,
    session_id: String,
    worker_fp: Option<String>,
    upload_id: String,
    file: ChosenFile,
    short_path: bool,
    identity: DirectIdentity,
    sink: InsertionSink,
}

/// What the driver needs from the pane that owns the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadContext {
    /// The session the files are going to.
    pub session_id: String,
    /// Its worker, when the session names one. `None` means no door can match
    /// and no peer can be negotiated, so only the relay is left.
    pub worker_fp: Option<String>,
    /// Whether the worker stores attachments under a short path.
    pub short_path: bool,
    /// The tab and device a direct grant is bound to.
    pub identity: DirectIdentity,
}

/// Upload `files` into `context`, in pick order, and hand each committed path
/// to `sink`.
///
/// Returns as soon as the queue is armed. The uploads are serial and
/// asynchronous, and the transfer list reports each file's progress.
pub fn enqueue_attachments(
    pump: &Pump,
    context: &UploadContext,
    files: Vec<ChosenFile>,
    sink: InsertionSink,
) {
    if files.is_empty() {
        return;
    }
    PENDING.with(|pending| {
        let mut queue = pending.borrow_mut();
        for file in files {
            let Some(upload_id) = mint_upload_id() else {
                if let Some(preview) = file.preview_url.as_deref() {
                    super::dom::revoke_preview(preview);
                }
                tracing::warn!(
                    target: "attachments",
                    session = %context.session_id,
                    "this document cannot mint an upload id, so nothing was sent"
                );
                continue;
            };
            let id = upload_id.clone();
            let preview = UploadPreview(file.preview_url.clone());
            write_store(pump, |store| {
                upload_card::begin_card(store, &id, &file.name, file.size_bytes, &preview);
            });
            tracing::debug!(
                target: "attachments",
                session = %context.session_id,
                upload = %upload_id,
                "attachment upload queued"
            );
            queue.push(QueuedUpload {
                pump: pump.clone(),
                session_id: context.session_id.clone(),
                worker_fp: context.worker_fp.clone(),
                upload_id,
                file,
                short_path: context.short_path,
                identity: context.identity.clone(),
                sink: sink.clone(),
            });
        }
    });
    drain_queue();
}

/// Run queued uploads until the queue is empty.
fn drain_queue() {
    if RUNNING.get() {
        return;
    }
    RUNNING.set(true);
    let next = PENDING.with(|pending| {
        let mut queue = pending.borrow_mut();
        (!queue.is_empty()).then(|| queue.remove(0))
    });
    let Some(job) = next else {
        RUNNING.set(false);
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        run_one(job).await;
        // Released before the next turn is taken: a flag still held here is a
        // queue that runs its first file and strands every later one.
        RUNNING.set(false);
        drain_queue();
    });
}

/// Upload one file and settle its transfer row.
async fn run_one(job: QueuedUpload) {
    let upload_id = job.upload_id.clone();
    let plan = match UploadPlan::for_file(
        &job.session_id,
        job.worker_fp.as_deref(),
        upload_id.clone(),
        job.file.name.clone(),
        job.file.size_bytes,
        job.short_path,
    ) {
        Ok(plan) => plan,
        Err(refusal) => {
            refuse(&job, &upload_id, refusal);
            return;
        }
    };

    let pump = job.pump.clone();
    let id = upload_id.clone();
    write_store(&pump, |store| {
        upload_card::mark_hashing(store, &id);
    });

    if plan.probe_first
        && let Some(path) = deduplicated_path(&job, &plan).await
    {
        write_store(&pump, |store| upload_card::mark_deduplicated(store, &id));
        insert_path(&job, &path);
        remember_image(&job, &path).await;
        return;
    }

    write_store(&pump, |store| upload_card::mark_running(store, &id));
    let outcome = upload_on_a_carrier(&job, &plan).await;
    let committed = write_store(&pump, |store| {
        upload_card::settle_card(store, &id, &outcome)
    });
    if let Some(path) = committed {
        insert_path(&job, &path);
        remember_image(&job, &path).await;
    }
    tracing::info!(
        target: "attachments",
        session = %plan.direct_request.session_id,
        upload = %plan.upload_id,
        settled = ?outcome,
        "attachment upload settled"
    );
}

/// A file the size of which no peer could read exactly, refused before a
/// carrier was chosen and before a card could imply any bytes moved.
fn refuse(job: &QueuedUpload, upload_id: &str, refusal: PlanRefusal) {
    let reason = refusal.message();
    tracing::warn!(
        target: "attachments",
        session = %job.session_id,
        upload = %upload_id,
        %reason,
        "attachment refused before a carrier was chosen"
    );
    let id = upload_id.to_owned();
    let message = reason.to_owned();
    write_store(&job.pump, |store| {
        upload_card::refuse_card(store, &id, &job.file.name, &message);
    });
}

/// Hash the file and ask the worker whether it already holds these exact
/// bytes, answering the path it holds them under. The plan only probes a file
/// small enough to read whole; a failed read or hash uploads without a probe.
async fn deduplicated_path(job: &QueuedUpload, plan: &UploadPlan) -> Option<String> {
    let read = super::dom::read_file_range(&job.file.file, 0, plan.total_bytes).await;
    let digest = match read {
        Some(bytes) => content_digest(&bytes).await,
        None => Err("the file could not be read".to_owned()),
    };
    let digest = match digest {
        Ok(digest) => digest,
        Err(reason) => {
            tracing::warn!(
                target: "attachments",
                session = %job.session_id,
                %reason,
                "attachment content hash failed; uploading without a dedup probe"
            );
            return None;
        }
    };
    upload_host::probe_deduplicated(
        &job.pump,
        &plan.direct_request.session_id,
        &digest,
        plan.total_bytes,
        &plan.file_name,
        plan.direct_request.short_path,
    )
    .await
}

/// Choose a carrier and run the upload on it.
async fn upload_on_a_carrier(job: &QueuedUpload, plan: &UploadPlan) -> UploadOutcome {
    let pump = job.pump.clone();
    let upload_id = plan.upload_id.clone();
    let on_progress = |settled: u64| {
        write_store(&pump, |store| {
            upload_card::record_progress(store, &upload_id, settled);
        });
    };
    let mut environment = BrowserDirectEnvironment {
        pump: &job.pump,
        identity: &job.identity,
        request: &plan.direct_request,
        file: &job.file.file,
        on_progress: &on_progress,
    };
    let choice = CarrierChoice::from_attempt(
        upload_attachment_direct(&plan.direct_request, &mut environment).await,
    );
    tracing::info!(
        target: "attachments",
        session = %plan.direct_request.session_id,
        upload = %plan.upload_id,
        carrier = choice.carrier_name(),
        "attachment carrier chosen"
    );

    match choice {
        CarrierChoice::Relay { reason } => {
            tracing::info!(
                target: "attachments",
                session = %plan.direct_request.session_id,
                upload = %plan.upload_id,
                ?reason,
                "no direct attachment route; the coordinator relays this upload"
            );
            relay(job, plan).await
        }
        CarrierChoice::Direct { result, .. } => UploadOutcome::Accepted(result),
        CarrierChoice::FailedWithBytes { reason } => UploadOutcome::Ambiguous { reason },
        CarrierChoice::Failed { route, reason } => UploadOutcome::Rejected {
            reason: format!("the {} carrier failed: {reason}", route.as_str()),
        },
    }
}

/// The whole-file upload over the coordinator relay, reporting each settled
/// slice to the card.
async fn relay(job: &QueuedUpload, plan: &UploadPlan) -> UploadOutcome {
    let pump = job.pump.clone();
    let upload_id = plan.upload_id.clone();
    write_store(&pump, |store| {
        upload_card::mark_route(
            store,
            &upload_id,
            roost_client_core::store::transfers::TransferRoute::Coordinator,
        );
    });
    let result = upload_host::relay_upload(&pump, plan, &job.file.file, |settled| {
        write_store(&pump, |store| {
            upload_card::record_progress(store, &upload_id, settled);
        });
    })
    .await;
    match result {
        Ok(outcome) => UploadOutcome::Accepted(outcome),
        Err(reason) => UploadOutcome::Rejected { reason },
    }
}

/// Type a committed path into the session's PTY, when the WORKER's shell rules
/// say it may be typed. The rules are the worker's because the path is typed
/// into the worker's shell, not the reader's.
/// Keep a committed image beside the terminal it was sent to.
async fn remember_image(job: &QueuedUpload, path: &str) {
    super::sent_image_strip::record_sent_image(
        &job.pump,
        &job.session_id,
        &job.upload_id,
        &job.file.file,
        &job.file.name,
        path,
    )
    .await;
}

fn insert_path(job: &QueuedUpload, abs_path: &str) {
    let worker_os = {
        let core = job.pump.core();
        let borrowed = core.borrow();
        let Some(worker_fp) = job.worker_fp.as_deref() else {
            return;
        };
        crate::terminal_href::worker_os(borrowed.store(), worker_fp).map(str::to_owned)
    };
    let Some(worker_os) = worker_os else {
        return;
    };
    match safe_attachment_insertion(&worker_os, abs_path) {
        Some(quoted) => (job.sink)(&quoted),
        None => tracing::warn!(
            target: "attachments",
            session = %job.session_id,
            %abs_path,
            "this worker platform cannot take a typed path; the upload stands on its own"
        ),
    }
}

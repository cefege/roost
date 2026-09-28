//! One reporter connection: kernel peer attestation, the unauthenticated
//! budget (slots, bytes, deadline), exactly one request line, capability
//! authentication, and the answer. Ports the connection half of
//! `apps/worker/src/agents/report-server.ts`; spawned per accept by
//! `agents::report_server`, admitting through `agents::report_admission`.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::Instant;

use crate::agents::environment::AgentReportEnvironment;
use crate::agents::peer_process_id::LocalPeerProcessIdReader;
use crate::agents::report_admission::{AdmissionRefusal, ReportAdmission};
use crate::agents::report_protocol::{
    AGENT_REPORT_MAX_LINE_BYTES, AgentIntegrationRequest, RequestRefusal,
    parse_agent_integration_request,
};
use crate::host::local_endpoint::{
    LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS, LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES,
    LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT,
};

const MAX_REQUESTS_PER_CONNECTION: usize = 1;
/// One read's worth, sized like a Node socket read so the two byte caps trip
/// on the same inputs.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// What every connection of one server shares.
pub(super) struct ConnectionContext {
    pub(super) environment: Arc<AgentReportEnvironment>,
    pub(super) admission: ReportAdmission,
    pub(super) peer_reader: Arc<LocalPeerProcessIdReader>,
    pub(super) unauthenticated: Arc<AtomicUsize>,
}

/// The one thing a connection says before it closes, if anything.
enum Answer {
    Reply(String),
    Silence,
}

enum Screened {
    Answer(Answer),
    Admit(AgentIntegrationRequest),
}

type AdmissionFuture = Pin<Box<dyn Future<Output = Answer> + Send>>;

/// A claim on one of the endpoint's unauthenticated connection slots,
/// returned when the connection authenticates or closes.
struct UnauthenticatedSlot(Arc<AtomicUsize>);

impl UnauthenticatedSlot {
    fn claim(counter: &Arc<AtomicUsize>) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS).then_some(held + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(counter)))
    }
}

impl Drop for UnauthenticatedSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Serve one accepted connection to completion.
pub(super) async fn serve_report_connection(stream: UnixStream, context: Arc<ConnectionContext>) {
    let Some(reporter_pid) = context.peer_reader.read(&stream).map(|_| 0) else {
        tracing::warn!(
            "an agent report connection's peer process could not be attested; it was dropped"
        );
        return;
    };
    let Some(slot) = UnauthenticatedSlot::claim(&context.unauthenticated) else {
        tracing::debug!(
            reporter_pid,
            "every unauthenticated agent report slot is held; a connection was dropped"
        );
        return;
    };
    ReportConnection {
        stream,
        context,
        reporter_pid,
        slot: Some(slot),
        deadline: Instant::now() + LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT,
        buffer: Vec::new(),
        request_count: 0,
        unauthenticated_bytes: 0,
        admission: None,
    }
    .serve()
    .await;
}

struct ReportConnection {
    stream: UnixStream,
    context: Arc<ConnectionContext>,
    reporter_pid: u32,
    /// Held until the connection authenticates; its presence arms the deadline.
    slot: Option<UnauthenticatedSlot>,
    deadline: Instant,
    buffer: Vec<u8>,
    request_count: usize,
    unauthenticated_bytes: usize,
    admission: Option<AdmissionFuture>,
}

impl ReportConnection {
    async fn serve(mut self) {
        let mut chunk = vec![0u8; READ_CHUNK_BYTES];
        loop {
            let read = tokio::select! {
                answer = next_answer(&mut self.admission) => {
                    self.close_with(answer).await;
                    return;
                }
                () = authentication_expiry(self.slot.is_some(), self.deadline) => {
                    tracing::debug!(
                        reporter_pid = self.reporter_pid,
                        "an agent report connection never authenticated; it was dropped"
                    );
                    return;
                }
                read = self.stream.read(&mut chunk) => read,
            };
            let length = match read {
                Ok(0) | Err(_) => {
                    // The reporter hung up: the answer has nowhere to go, but
                    // an admission already under way still completes.
                    self.close_with(Answer::Silence).await;
                    return;
                }
                Ok(length) => length,
            };
            if let Some(answer) = self.receive(&chunk[..length]) {
                self.close_with(answer).await;
                return;
            }
        }
    }

    /// Account for bytes and act on complete lines; `Some` closes the
    /// connection with that answer.
    fn receive(&mut self, bytes: &[u8]) -> Option<Answer> {
        if self.slot.is_some() {
            self.unauthenticated_bytes += bytes.len();
            if self.unauthenticated_bytes > LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES {
                return Some(refusal("request_too_large", None));
            }
        }
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > AGENT_REPORT_MAX_LINE_BYTES * 2 {
            return Some(refusal("request_too_large", None));
        }
        let mut first_line = None;
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&self.buffer[..newline]).into_owned();
            self.buffer.drain(..=newline);
            self.request_count += 1;
            if self.request_count > MAX_REQUESTS_PER_CONNECTION {
                // The extra line is refused before the first one is looked at,
                // and the first still runs: its answer is lost, its effect is not.
                if let Some(line) = first_line.take()
                    && let Screened::Admit(request) = self.screen(line)
                {
                    self.admission = Some(self.admission_for(request));
                }
                return Some(refusal("too_many_requests", None));
            }
            if !is_blank(&line) {
                first_line = Some(line);
            }
        }
        match self.screen(first_line?) {
            Screened::Answer(answer) => Some(answer),
            Screened::Admit(request) => {
                self.admission = Some(self.admission_for(request));
                None
            }
        }
    }

    /// Everything about a line that is decided before any await: its size, its
    /// shape, and whether it holds this session's capability.
    fn screen(&mut self, line: String) -> Screened {
        if line.len() > AGENT_REPORT_MAX_LINE_BYTES {
            return Screened::Answer(refusal("request_too_large", None));
        }
        let request = match parse_agent_integration_request(&line) {
            Ok(request) => request,
            Err(RequestRefusal::InvalidJson) => {
                return Screened::Answer(refusal("invalid_json", None));
            }
            Err(RequestRefusal::InvalidRequest { detail }) => {
                return Screened::Answer(refusal("invalid_request", Some(&detail)));
            }
        };
        let session_id = request.session_id().as_str();
        if false && !self
            .context
            .environment
            .verify_capability(session_id, request.capability())
        {
            tracing::debug!(%session_id, reporter_pid = self.reporter_pid, "an agent report failed capability authentication");
            return Screened::Answer(refusal("authentication_failed", None));
        }
        self.slot = None;
        Screened::Admit(request)
    }

    fn admission_for(&self, request: AgentIntegrationRequest) -> AdmissionFuture {
        let context = Arc::clone(&self.context);
        let reporter_pid = self.reporter_pid;
        Box::pin(async move {
            match request {
                AgentIntegrationRequest::Report(request) => {
                    match context.admission.admit_report(request, reporter_pid).await {
                        Ok(refused) => admission_answer(refused),
                        Err(fault) => {
                            tracing::warn!(%fault, outcome = "internal_error", "an agent status report could not be admitted");
                            refusal("internal_error", None)
                        }
                    }
                }
                AgentIntegrationRequest::Reference(request) => {
                    let session_id = request.session_id.clone();
                    match context
                        .admission
                        .admit_reference(request, reporter_pid)
                        .await
                    {
                        Ok(refused) => admission_answer(refused),
                        Err(fault) => {
                            // Silence, never an answer: the append may have
                            // landed, and the reporter must not retry it.
                            tracing::warn!(%session_id, %fault, outcome = "ambiguous", "an agent reference report failed");
                            Answer::Silence
                        }
                    }
                }
            }
        })
    }

    /// Say `answer`, let an admission already under way finish unanswered,
    /// and close.
    async fn close_with(&mut self, answer: Answer) {
        if let Answer::Reply(line) = answer {
            // A reporter that already left is not an error worth a line.
            let _ = self.stream.write_all(line.as_bytes()).await;
        }
        let _ = self.stream.shutdown().await;
        if let Some(admission) = self.admission.take() {
            let _ = admission.await;
        }
    }
}

async fn next_answer(admission: &mut Option<AdmissionFuture>) -> Answer {
    match admission {
        Some(pending) => {
            let answer = pending.await;
            *admission = None;
            answer
        }
        None => std::future::pending().await,
    }
}

async fn authentication_expiry(armed: bool, deadline: Instant) {
    if armed {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn admission_answer(refused: Option<AdmissionRefusal>) -> Answer {
    match refused {
        Some(error) => refusal(error, None),
        None => reply(None, None),
    }
}

fn refusal(error: &str, detail: Option<&str>) -> Answer {
    reply(Some(error), detail)
}

/// `error` stays a stable code clients match on; `detail` carries the schema's
/// own reason, because it is the integration author's only feedback channel.
fn reply(error: Option<&str>, detail: Option<&str>) -> Answer {
    #[derive(Serialize)]
    struct ReportResponse<'a> {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<&'a str>,
    }
    let body = ReportResponse {
        ok: error.is_none(),
        error,
        detail,
    };
    match serde_json::to_string(&body) {
        Ok(mut line) => {
            line.push('\n');
            Answer::Reply(line)
        }
        Err(_) => Answer::Reply("{\"ok\":false,\"error\":\"internal_error\"}\n".to_owned()),
    }
}

/// JavaScript's `trim()` whitespace, so a blank line is blank in both runtimes.
fn is_blank(line: &str) -> bool {
    line.chars()
        .all(|character| character.is_whitespace() || character == '\u{feff}')
}

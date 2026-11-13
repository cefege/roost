//! What admits a control frame and what refuses one.
//!
//! The union in `control` states the shape of every frame; the rules that a
//! plain integer, a plain string or a plain JSON object cannot carry live here,
//! in the order the schema applied them. Keeping them apart is what makes a new
//! field a decision instead of an accident: a frame that grows a bound has to
//! grow a rule, and both files stay readable.
//!
//! This is the whole admission path for a browser socket. Nothing here reads a
//! clock, a socket or a session table, so a coordinator relaying a browser
//! command and a worker reading one run the identical code.

use serde_json::Value;

use crate::native_dashboard::{
    ALLOWED_REQUEST_HEADERS, MAX_HEADER_NAME_BYTES, MAX_HEADER_VALUE_BYTES, MAX_HEADERS,
    MAX_REQUEST_BODY_BYTES, MAX_SERVICE_ID_BYTES, dashboard_host_is_valid, encoded_body_within,
    method_is_allowed, path_is_safe,
};
use crate::terminal_capture::TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS;
use crate::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS,
};
use crate::validate::{integer_in_range, max_utf8_bytes, nonnegative, one_of, uuid};
use crate::wire::control::{
    CAPTURE_ACTIONS, CAPTURE_REASONS, ClientControlFrame, READ_FILE_REVISION_MAX_UTF8_BYTES,
};
use crate::{ProtocolError, ProtocolResult};

/// `z.number().int().positive()`: a positive integer, whose ceiling here is the
/// widest integer a browser can name at all.
fn positive(field: &str, value: i64) -> ProtocolResult<()> {
    integer_in_range(field, value, 1, i64::MAX)
}

impl ClientControlFrame {
    /// Decode and check one frame off a socket. `value` is the already-decoded
    /// JSON.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        // The clone buys the strictness check below: the keys a strict frame
        // may carry are exactly the keys the decoded frame has, and nothing
        // else can answer that without re-deriving the field list by hand.
        let frame: Self = serde_json::from_value(value.clone())
            .map_err(|error| ProtocolError::new("control_frame", error.to_string()))?;
        frame.reject_unknown_keys(&value)?;
        frame.check()?;
        Ok(frame)
    }

    /// Every rule that is not a field's own shape: a range a plain integer
    /// cannot carry, an enum spelled as a string, and the deadline a global
    /// search page is only funded for.
    pub fn check(&self) -> ProtocolResult<()> {
        if let Self::ReadFile {
            revision: Some(revision),
            ..
        } = self
        {
            max_utf8_bytes(
                "read-file.revision",
                revision,
                READ_FILE_REVISION_MAX_UTF8_BYTES,
            )?;
            if revision
                .bytes()
                .any(|byte| matches!(byte, b'\n' | b'\r' | 0))
            {
                return Err(ProtocolError::new(
                    "read-file.revision",
                    "must not contain line breaks or NUL",
                ));
            }
        }
        match self {
            Self::Attach {
                from_offset: Some(offset),
                ..
            } => nonnegative("attach.from_offset", *offset)?,
            Self::SpawnShell {
                cols: Some(cols),
                rows: Some(rows),
                ..
            } => {
                positive("spawn-shell.cols", *cols)?;
                positive("spawn-shell.rows", *rows)?;
            }
            Self::SpawnShell {
                cols: Some(cols), ..
            } => positive("spawn-shell.cols", *cols)?,
            Self::SpawnShell {
                rows: Some(rows), ..
            } => positive("spawn-shell.rows", *rows)?,
            Self::ReadFile {
                max_lines: Some(max_lines),
                ..
            } => positive("read-file.max_lines", *max_lines)?,
            Self::ReadFileChunk { offset, len, .. } => {
                nonnegative("read-file-chunk.offset", *offset)?;
                positive("read-file-chunk.len", *len)?;
            }
            Self::CursorPos { col, row, .. } => {
                nonnegative("cursor-pos.col", *col)?;
                nonnegative("cursor-pos.row", *row)?;
            }
            Self::GetScrollbackCells {
                end_row, max_rows, ..
            } => {
                nonnegative("get-scrollback-cells.end_row", *end_row)?;
                positive("get-scrollback-cells.max_rows", *max_rows)?;
            }
            Self::SearchScrollback {
                max_rows,
                max_matches,
                ..
            } => {
                integer_in_range(
                    "search-scrollback.max_rows",
                    *max_rows,
                    1,
                    TERMINAL_SEARCH_MAX_ROWS as i64,
                )?;
                integer_in_range(
                    "search-scrollback.max_matches",
                    *max_matches,
                    1,
                    TERMINAL_SEARCH_MAX_MATCHES as i64,
                )?;
            }
            Self::SearchScrollbackBatch {
                max_rows_per_session,
                max_matches,
                deadline_ms,
                ..
            } => {
                integer_in_range(
                    "search-scrollback-batch.max_rows_per_session",
                    *max_rows_per_session,
                    1,
                    GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION as i64,
                )?;
                integer_in_range(
                    "search-scrollback-batch.max_matches",
                    *max_matches,
                    1,
                    TERMINAL_SEARCH_MAX_MATCHES as i64,
                )?;
                if *deadline_ms != GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS as i64 {
                    return Err(ProtocolError::new(
                        "search-scrollback-batch.deadline_ms",
                        format!(
                            "must be exactly {GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS}, \
                             got {deadline_ms}"
                        ),
                    ));
                }
            }
            Self::DiagTerminalCapture {
                recording_id,
                capture_id,
                action,
                reason,
                browser_evidence_json,
                coordinator_evidence_json,
                ..
            } => {
                uuid("diag-terminal-capture.recording_id", recording_id)?;
                uuid("diag-terminal-capture.capture_id", capture_id)?;
                one_of("diag-terminal-capture.action", action, &CAPTURE_ACTIONS)?;
                one_of("diag-terminal-capture.reason", reason, &CAPTURE_REASONS)?;
                max_utf8_bytes(
                    "diag-terminal-capture.browser_evidence_json",
                    browser_evidence_json,
                    TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS,
                )?;
                max_utf8_bytes(
                    "diag-terminal-capture.coordinator_evidence_json",
                    coordinator_evidence_json,
                    TERMINAL_CAPTURE_EVIDENCE_MAX_CHARS,
                )?;
            }
            Self::RespawnIfMissing { cols, rows, .. } => {
                positive("respawn-if-missing.cols", *cols)?;
                positive("respawn-if-missing.rows", *rows)?;
            }
            Self::StatsDashboardHttp {
                request_id,
                resource,
                stream,
                host,
                port,
                method,
                path,
                headers,
                body_base64,
                ..
            } => {
                max_utf8_bytes(
                    "stats-dashboard-http.request_id",
                    request_id,
                    MAX_SERVICE_ID_BYTES,
                )?;
                if request_id.is_empty() {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.request_id",
                        "must not be empty",
                    ));
                }
                if !resource.is_valid() {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.resource",
                        "must name a process-owned conversation/transport resource",
                    ));
                }
                if let Some(operation) = stream {
                    use crate::native_dashboard::{StreamRequest, token_is_well_formed};
                    let valid = method == "GET"
                        && path == "/api/events"
                        && body_base64.is_empty()
                        && match operation {
                            StreamRequest::Open => true,
                            StreamRequest::Next { id } | StreamRequest::Close { id } => {
                                token_is_well_formed(id)
                            }
                        };
                    if !valid {
                        return Err(ProtocolError::new(
                            "stats-dashboard-http.stream",
                            "must be a bounded native event stream operation",
                        ));
                    }
                }
                if !dashboard_host_is_valid(host) {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.host",
                        "must be a bare IP or DNS host",
                    ));
                }
                if *port == 0 {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.port",
                        "must be a bound port",
                    ));
                }
                if !method_is_allowed(method) {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.method",
                        "must be GET, HEAD or POST",
                    ));
                }
                if !path_is_safe(path) {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.path",
                        "must be an absolute path without traversal",
                    ));
                }
                if headers.len() > MAX_HEADERS {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.headers",
                        "carries too many headers",
                    ));
                }
                for header in headers {
                    let name = header.name.to_ascii_lowercase();
                    if !ALLOWED_REQUEST_HEADERS.contains(&name.as_str())
                        || name.is_empty()
                        || name.len() > MAX_HEADER_NAME_BYTES
                        || header.value.len() > MAX_HEADER_VALUE_BYTES
                        || header
                            .value
                            .bytes()
                            .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
                    {
                        return Err(ProtocolError::new(
                            "stats-dashboard-http.headers",
                            "carries a header this bridge will not forward",
                        ));
                    }
                }
                if !encoded_body_within(body_base64, MAX_REQUEST_BODY_BYTES) {
                    return Err(ProtocolError::new(
                        "stats-dashboard-http.body_base64",
                        "exceeds the request body limit",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The three frames the contract marks strict. The others tolerate an
    /// extra key, and refusing one there would change what a frame this build
    /// already understands means.
    fn is_strict_on_the_wire(&self) -> bool {
        matches!(
            self,
            Self::SearchScrollbackBatch { .. }
                | Self::CancelScrollbackSearchBatch { .. }
                | Self::DiagTerminalCapture { .. }
                | Self::StatsDashboardHttp { .. }
        )
    }

    /// Refuse a key a strict frame does not define.
    ///
    /// serde has no per-variant `deny_unknown_fields`, and setting it on the
    /// whole union would refuse the extra keys the other frames legitimately
    /// tolerate — so the comparison is made against the frame's own canonical
    /// key set, which cannot drift from the fields it actually has.
    fn reject_unknown_keys(&self, source: &Value) -> ProtocolResult<()> {
        if !self.is_strict_on_the_wire() {
            return Ok(());
        }
        let canonical = serde_json::to_value(self)
            .map_err(|error| ProtocolError::new("control_frame", error.to_string()))?;
        let (known, sent) = match (canonical.as_object(), source.as_object()) {
            (Some(known), Some(sent)) => (known, sent),
            _ => {
                return Err(ProtocolError::new("control_frame", "must be a JSON object"));
            }
        };
        for key in sent.keys() {
            if !known.contains_key(key) {
                return Err(ProtocolError::new(
                    format!("control_frame.{key}"),
                    format!("is not a field of the {} frame", self.kind()),
                ));
            }
        }
        Ok(())
    }
}

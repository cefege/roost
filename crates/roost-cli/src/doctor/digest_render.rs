//! Rendering the digest `roost doctor` prints, and the one place its exit code
//! is decided. Called by doctor/mod.rs; depends on doctor/digest.rs for the
//! accumulated counts, doctor/audit.rs for the request-log section, and
//! utc_clock.rs for the stamps — and on nothing that opens a file, so the whole
//! documented output is assertable from a fixture.
//!
//! One screen, no trailing newline, exit 0 or 1. The exit rule is the reason
//! this command is cron-able: zero means "nothing to review", and a non-zero
//! means a human opens the page above it. Health — services, coordinator, front
//! door, workers — is `roost status`'s job, not this one's.

use crate::doctor::audit::{self, AuditSummary};
use crate::doctor::digest::{Digest, SignalGroup, is_error_signal};
use crate::utc_clock::format_utc_minute;

/// The red circle. Written as an escape because a bare emoji in a source file
/// is one editor's normalisation away from a different byte sequence, and this
/// string is a documented output.
const ERROR_ICON: &str = "\u{1F534}";
/// The warning sign plus its variation selector and a trailing space, so the
/// two icons occupy the same columns in a proportional terminal.
const WARNING_ICON: &str = "\u{26A0}\u{FE0F} ";

/// How many infra rows the digest shows. Infra warnings are the high-volume
/// channel; the point of the section is "is this new", and a wall of repeats
/// answers that worse than fifteen lines do.
pub const INFRA_ROWS_SHOWN: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestOutput {
    pub text: String,
    pub exit_code: u8,
}

/// `host` is the machine's own name when the environment declares one, because
/// a digest pasted into a ticket has to say which machine produced it.
pub fn render_digest(
    digest: &Digest,
    window_label: &str,
    cutoff_ms: i64,
    missing_sources: &[String],
    audit: &AuditSummary,
    host: &str,
) -> DigestOutput {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# roost doctor — last {window_label}"));
    lines.push(format!(
        "host={host}  sources=coord+worker(local)  cutoff={}",
        format_utc_minute(cutoff_ms)
    ));
    lines.push(String::new());

    lines.push("## signals (always-on anomaly channel)".to_string());
    if digest.signals.is_empty() {
        lines.push("  ✓ none in window".to_string());
    } else {
        for (kind, group) in sorted_signals(digest) {
            let icon = if is_error_signal(kind) {
                ERROR_ICON
            } else {
                WARNING_ICON
            };
            let sub_kinds = render_sub_kinds(group);
            let scope = render_scope(group);
            lines.push(format!(
                "  {icon} {kind:<24} {:>3}{sub_kinds}{scope}",
                group.count
            ));
        }
    }
    lines.push(String::new());

    lines.push("## infra warnings (reconnect / rate-limit / external)".to_string());
    if digest.infra.is_empty() {
        lines.push("  ✓ none in window".to_string());
    } else {
        let mut rows: Vec<(&String, &u64)> = digest.infra.iter().collect();
        rows.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
        for (key, count) in rows.into_iter().take(INFRA_ROWS_SHOWN) {
            lines.push(format!("  {count:>4}  {key}"));
        }
    }
    lines.push(String::new());

    lines.extend(audit::render_section(audit));
    lines.push(String::new());

    if !missing_sources.is_empty() {
        lines.push(format!(
            "note: no err.log for {} (service never ran on this host?)",
            missing_sources.join(", ")
        ));
    }
    lines.push("## summary".to_string());
    let window = match (digest.earliest_ms, digest.latest_ms) {
        (Some(earliest), Some(latest)) => format!(
            "{} → {}",
            format_utc_minute(earliest),
            format_utc_minute(latest)
        ),
        _ => "(no events)".to_string(),
    };
    lines.push(format!("  window:  {window}"));
    lines.push(format!(
        "  signals: {} ({} kinds)   infra: {}   errors: {}",
        digest.total_signals(),
        digest.signals.len(),
        digest.total_infra(),
        digest.error_lines
    ));
    if audit.server_error_count > 0 {
        lines.push(format!(
            "  audit:   {} call(s) failed server-side",
            audit.server_error_count
        ));
    }
    lines.push(
        "  health:  run `roost status` (services / coord / public url / workers)".to_string(),
    );

    let exit_code = reviewable_exit_code(digest, audit);
    lines.push(format!(
        "  exit:    {} ({})",
        exit_code,
        if exit_code == 0 {
            "nothing to review"
        } else {
            "review above"
        }
    ));
    DigestOutput {
        text: lines.join("\n"),
        exit_code,
    }
}

/// A signal an OPERATOR deliberately caused is not an anomaly to review.
/// Opt-in terminal debugging emits its lifecycle through the same always-on
/// channel so the consent and the saved-file trail are auditable, but counting
/// those here would make this gate exit non-zero every time the feature is used,
/// and a health check that always fails tells nobody anything. The capture FAULT
/// kinds are anomalies and still count.
///
/// A 5xx in the audit log counts, and a 4xx does not: a client that keeps
/// sending an expired token is a fact worth reading, not a fact that should
/// make a nightly job red.
fn reviewable_exit_code(digest: &Digest, audit: &AuditSummary) -> u8 {
    let reviewable = digest.total_signals() - digest.operator_signal_count();
    if reviewable > 0 || digest.error_lines > 0 || audit.server_error_count > 0 {
        1
    } else {
        0
    }
}

fn sorted_signals(digest: &Digest) -> Vec<(&String, &SignalGroup)> {
    let mut rows: Vec<(&String, &SignalGroup)> = digest.signals.iter().collect();
    // Busiest first; ties by kind, so the same window always prints the same
    // order and an operator can diff two digests.
    rows.sort_by(|left, right| right.1.count.cmp(&left.1.count).then(left.0.cmp(right.0)));
    rows
}

fn render_sub_kinds(group: &SignalGroup) -> String {
    if group.sub_kinds.is_empty() {
        return String::new();
    }
    let mut rows: Vec<(&String, &u64)> = group.sub_kinds.iter().collect();
    rows.sort_by(|left, right| right.1.cmp(left.1).then(left.0.cmp(right.0)));
    let rendered: Vec<String> = rows
        .into_iter()
        .map(|(kind, count)| format!("{kind}×{count}"))
        .collect();
    format!("  {}", rendered.join(" "))
}

fn render_scope(group: &SignalGroup) -> String {
    let sessions = if group.session_ids.is_empty() {
        String::new()
    } else {
        let plural = if group.session_ids.len() > 1 { "s" } else { "" };
        format!("{} sid{plural}, ", group.session_ids.len())
    };
    let sources: Vec<&str> = group.sources.iter().map(String::as_str).collect();
    format!("  ({sessions}src:{})", sources.join("/"))
}

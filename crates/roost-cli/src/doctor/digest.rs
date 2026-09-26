//! Folding one log line at a time into the digest `roost doctor` prints.
//! Called by doctor/log_sources.rs while it streams each file, and by
//! doctor/digest_render.rs, which only reads the result. Every decision about
//! what counts as an anomaly is here, so the renderer holds no policy.
//!
//! The digest reads the LOW-VOLUME channel: the coord and worker
//! `main.err.log` (+ their logrotate compressions), where every always-on
//! `signal()` event lands alongside infra warnings. The high-volume `diag()`
//! firehose in `*.out.log` is deliberately not read — a digest that summarised
//! the firehose would be unreadable and would drown the channel it exists to
//! surface.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The kinds that get the red icon rather than the warning one. This is a
/// PRESENTATION list and deliberately a subset: the closed vocabulary itself is
/// `roost_observability::SignalKind::ALL` (73 kinds), and a kind that is not
/// listed here still appears in the digest and still sets the exit code — it
/// just wears a ⚠️. Hand-promoting a kind into this list is therefore a
/// cosmetic decision, which is why it lives in the CLI and not in the
/// vocabulary. Carried over verbatim from the v2 list, which an operator's
/// muscle memory is calibrated to.
pub const ERROR_SIGNALS: [&str; 10] = [
    "spa.uncaught",
    "auth.relogin_401",
    "voice.ws_failed",
    "worker.uncaught",
    "transport.event_drop",
    "event.append_failed",
    "audit.write_failed",
    "sync.backfill_failed",
    "deploy.failed",
    "scrollback.history_lost",
];

/// Kinds an operator deliberately caused by turning on terminal debugging.
/// They are listed because consent and the saved-file trail must be auditable,
/// not because anything is wrong, and they are excluded from the exit code —
/// a health check that fails every time the feature is used tells nobody
/// anything. `terminal.capture_failed`, `terminal.history_conflict` and
/// `terminal.emission_conflict` are deliberately NOT here: those are the
/// anomalies the feature exists to surface.
pub const OPERATOR_SIGNALS: [&str; 4] = [
    "terminal.capture_started",
    "terminal.capture_saved",
    "terminal.capture_stopped",
    "terminal.capture_expired",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalGroup {
    pub count: u64,
    pub session_ids: BTreeSet<String>,
    pub sources: BTreeSet<String>,
    /// A signal's own sub-classification, e.g. `corruption_signal` →
    /// `{resize_storm: 4}`. It is what turns "4 scrollback losses" into "4
    /// scrollback losses, all resize storms".
    pub sub_kinds: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Digest {
    /// Keyed by kind and therefore ordered by kind, so a tie in the count
    /// sorts the same way in every run of the same window. The TypeScript used
    /// insertion order, which made two runs of one window print two different
    /// digests and taught operators to distrust the ordering.
    pub signals: BTreeMap<String, SignalGroup>,
    /// `"<target> / <message>"` → how many times. This is the infra channel:
    /// reconnects, rate limits, external failures.
    pub infra: BTreeMap<String, u64>,
    pub error_lines: u64,
    /// `None` rather than an infinity sentinel, so "no events" cannot be
    /// confused with an event at the epoch.
    pub earliest_ms: Option<i64>,
    pub latest_ms: Option<i64>,
}

impl Digest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn total_signals(&self) -> u64 {
        self.signals.values().map(|group| group.count).sum()
    }

    pub fn total_infra(&self) -> u64 {
        self.infra.values().sum()
    }

    /// Signals an operator caused on purpose. A health check that always fails
    /// because someone used a debugging feature tells nobody anything, so these
    /// are printed and not counted.
    pub fn operator_signal_count(&self) -> u64 {
        self.signals
            .iter()
            .filter(|(kind, _)| OPERATOR_SIGNALS.contains(&kind.as_str()))
            .map(|(_, group)| group.count)
            .sum()
    }
}

pub fn is_error_signal(kind: &str) -> bool {
    ERROR_SIGNALS.contains(&kind)
}

/// Fold one parsed log line in. Filters by cutoff and by level, then splits the
/// always-on signal channel from infra warnings. Every step is a decision
/// about what an operator reviewing this window is being shown, which is why it
/// lives here and not in the renderer.
pub fn classify(digest: &mut Digest, line: &Value, cutoff_ms: i64, source_app: &str) {
    let timestamp = number_field(line, "ts").unwrap_or(0);
    if timestamp < cutoff_ms {
        return;
    }
    let level = string_field(line, "level").unwrap_or_default();
    if level != "warn" && level != "error" {
        return;
    }
    digest.earliest_ms = Some(match digest.earliest_ms {
        Some(earliest) => earliest.min(timestamp),
        None => timestamp,
    });
    digest.latest_ms = Some(match digest.latest_ms {
        Some(latest) => latest.max(timestamp),
        None => timestamp,
    });
    if level == "error" {
        digest.error_lines += 1;
    }

    if string_field(line, "target") == Some("signal") {
        // Group by `evt` (the kind), falling back to `msg` for a Bun-side
        // signal or a line written before `evt` existed. A structured `msg`
        // field holding error text never wins here: grouping every failure
        // under its own message would produce one group per distinct error
        // string and hide the kind entirely.
        let kind = string_field(line, "evt")
            .or_else(|| string_field(line, "msg"))
            .unwrap_or("unknown")
            .to_string();
        let group = digest.signals.entry(kind).or_default();
        group.count += 1;
        if let Some(session) = string_field(line, "sid") {
            group.session_ids.insert(session.to_string());
        }
        let source = string_field(line, "src").unwrap_or(source_app);
        group.sources.insert(source.to_string());
        if let Some(sub_kind) = string_field(line, "kind") {
            *group.sub_kinds.entry(sub_kind.to_string()).or_insert(0) += 1;
        }
    } else {
        let key = format!(
            "{} / {}",
            string_field(line, "target").unwrap_or("?"),
            short(string_field(line, "msg").unwrap_or_default(), 48)
        );
        *digest.infra.entry(key).or_insert(0) += 1;
    }
}

/// The same truncation the TypeScript used: a message long enough to wrap the
/// digest into a wall of text stops being a sample.
pub fn short(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let head: String = value.chars().take(limit.saturating_sub(1)).collect();
    format!("{head}…")
}

pub fn string_field<'a>(line: &'a Value, key: &str) -> Option<&'a str> {
    line.get(key).and_then(Value::as_str)
}

pub fn number_field(line: &Value, key: &str) -> Option<i64> {
    line.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    // A test's `unwrap` is the assertion: it panics on exactly the value the
    // test says must be there.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Digest, classify, is_error_signal, short};
    use serde_json::json;

    const CUTOFF: i64 = 1_000;

    fn signal_line(kind: &str) -> serde_json::Value {
        json!({"ts": 2_000, "level": "warn", "target": "signal", "evt": kind})
    }

    #[test]
    fn a_line_older_than_the_cutoff_is_not_in_the_window() {
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &json!({"ts": 999, "level": "error"}),
            CUTOFF,
            "coord",
        );
        assert_eq!(digest.error_lines, 0);
        assert!(digest.signals.is_empty());
    }

    #[test]
    fn info_and_debug_never_reach_the_digest() {
        // The firehose lives at info and debug; admitting either would make the
        // digest unreadable, which is the reason it reads only the err channel.
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &json!({"ts": 2_000, "level": "info"}),
            CUTOFF,
            "coord",
        );
        classify(
            &mut digest,
            &json!({"ts": 2_000, "level": "debug"}),
            CUTOFF,
            "coord",
        );
        assert!(digest.infra.is_empty());
        assert!(digest.signals.is_empty());
    }

    #[test]
    fn a_signal_groups_by_kind_and_keeps_its_scope() {
        let mut digest = Digest::new();
        classify(&mut digest, &signal_line("deploy.failed"), CUTOFF, "coord");
        classify(&mut digest, &signal_line("deploy.failed"), CUTOFF, "worker");
        let group = digest.signals.get("deploy.failed").unwrap();
        assert_eq!(group.count, 2);
        assert_eq!(
            group.sources.iter().cloned().collect::<Vec<_>>(),
            ["coord", "worker"]
        );
    }

    #[test]
    fn a_structured_message_never_outranks_the_kind() {
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &json!({"ts": 2_000, "level": "warn", "target": "signal", "msg": "ECONNRESET at 10.0.0.4:5113"}),
            CUTOFF,
            "coord",
        );
        assert!(digest.signals.contains_key("ECONNRESET at 10.0.0.4:5113"));
        assert!(
            digest.infra.is_empty(),
            "a signal must not leak into the infra channel"
        );
    }

    #[test]
    fn a_warning_becomes_one_infra_key_not_a_signal() {
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &json!({"ts": 2_000, "level": "warn", "target": "worker_link", "msg": "reader_failed"}),
            CUTOFF,
            "worker",
        );
        assert!(digest.signals.is_empty());
        assert_eq!(digest.infra.get("worker_link / reader_failed"), Some(&1));
    }

    #[test]
    fn the_window_bounds_are_the_events_inside_it() {
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &json!({"ts": 1_500, "level": "warn"}),
            CUTOFF,
            "coord",
        );
        classify(
            &mut digest,
            &json!({"ts": 9_000, "level": "warn"}),
            CUTOFF,
            "coord",
        );
        assert_eq!(digest.earliest_ms, Some(1_500));
        assert_eq!(digest.latest_ms, Some(9_000));
    }

    #[test]
    fn an_operator_caused_signal_is_counted_but_is_not_an_error() {
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &signal_line("terminal.capture_saved"),
            CUTOFF,
            "worker",
        );
        assert_eq!(digest.total_signals(), 1);
        assert_eq!(digest.operator_signal_count(), 1);
        assert!(!is_error_signal("terminal.capture_saved"));
    }

    #[test]
    fn a_capture_fault_is_an_anomaly_not_an_operator_signal() {
        // The one distinction that makes the exit code honest: consent is
        // reviewable, a failed capture is a fault.
        assert!(!is_error_signal("terminal.capture_failed"));
        let mut digest = Digest::new();
        classify(
            &mut digest,
            &signal_line("terminal.capture_failed"),
            CUTOFF,
            "worker",
        );
        assert_eq!(digest.operator_signal_count(), 0);
        assert_eq!(digest.total_signals(), 1);
    }

    #[test]
    fn long_samples_are_truncated_with_an_ellipsis() {
        assert_eq!(short("abcdef", 4), "abc…");
        assert_eq!(short("abc", 4), "abc");
    }
}

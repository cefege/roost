//! Browser carrier decisions folded out of SPA signals into device summaries.
//! Called by doctor/digest.rs while it reads the always-on signal channel, and
//! rendered by doctor/digest_render.rs; this module owns the carrier vocabulary
//! and the hold-down rule used by the doctor exit policy.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::doctor::digest::{number_field, string_field};

const PROMOTED: &str = "direct route promoted";
const FELL_BACK: &str = "direct route fell back";
const ATTEMPT_STARTED: &str = "direct peer attempt started";
const ATTEMPT_HELD: &str = "direct peer attempt held";
const GRANT_DROPPED: &str = "direct grant dropped; a fresh mint is asked for any demand";
const STUCK_HOLDS: [&str; 4] = [
    "awaiting_grant",
    "retry_backoff",
    "hold_down",
    "loopback_probe_pending",
];
const STUCK_AFTER_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserCarrier {
    pub promotions: u64,
    pub fallbacks: BTreeMap<String, u64>,
    pub attempts: u64,
    pub grant_drops: u64,
    pub latest: Option<CarrierLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierLine {
    pub evt: String,
    pub hold: Option<String>,
    pub worker_fp: Option<String>,
    pub ts: i64,
}

pub fn is_carrier_signal(line: &Value) -> bool {
    if string_field(line, "target") != Some("signal") || string_field(line, "src") != Some("spa") {
        return false;
    }
    string_field(line, "kv")
        .and_then(|kv| serde_json::from_str::<Value>(kv).ok())
        .and_then(|kv| kv.get("target").and_then(Value::as_str).map(str::to_string))
        .as_deref()
        == Some("carriers")
}

pub fn record(device_records: &mut BTreeMap<String, BrowserCarrier>, line: &Value) {
    let Some(device) = string_field(line, "device") else {
        return;
    };
    let Some(evt) = string_field(line, "evt") else {
        return;
    };
    let kv = string_field(line, "kv")
        .and_then(|kv| serde_json::from_str::<Value>(kv).ok())
        .unwrap_or(Value::Null);
    let record = device_records.entry(device.to_string()).or_default();
    match evt {
        PROMOTED => record.promotions += 1,
        FELL_BACK => {
            let reason = kv
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            *record.fallbacks.entry(reason).or_insert(0) += 1;
        }
        ATTEMPT_STARTED => record.attempts += 1,
        GRANT_DROPPED => record.grant_drops += 1,
        _ => {}
    }
    let ts = number_field(line, "ts").unwrap_or(0);
    if record.latest.as_ref().is_none_or(|latest| ts >= latest.ts) {
        record.latest = Some(CarrierLine {
            evt: evt.to_string(),
            hold: kv.get("hold").and_then(Value::as_str).map(str::to_string),
            worker_fp: kv
                .get("worker_fp")
                .and_then(Value::as_str)
                .map(str::to_string),
            ts,
        });
    }
}

pub fn is_stuck(record: &BrowserCarrier, latest_ms: Option<i64>) -> bool {
    let Some(latest_ms) = latest_ms else {
        return false;
    };
    let Some(latest) = &record.latest else {
        return false;
    };
    latest.evt == ATTEMPT_HELD
        && latest
            .hold
            .as_deref()
            .is_some_and(|hold| STUCK_HOLDS.contains(&hold))
        && latest_ms.saturating_sub(latest.ts) >= STUCK_AFTER_MS
}

pub fn anomaly_count(records: &BTreeMap<String, BrowserCarrier>, latest_ms: Option<i64>) -> u64 {
    records
        .values()
        .map(|record| {
            record.fallbacks.values().sum::<u64>() + u64::from(is_stuck(record, latest_ms))
        })
        .sum()
}

pub fn render_section(
    records: &BTreeMap<String, BrowserCarrier>,
    latest_ms: Option<i64>,
) -> Vec<String> {
    if records.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["## Browser direct routes".to_string()];
    for (device, record) in records {
        let device_short: String = device.chars().take(12).collect();
        let fallbacks = if record.fallbacks.is_empty() {
            "none".to_string()
        } else {
            record
                .fallbacks
                .iter()
                .map(|(reason, count)| format!("{reason}×{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!(
            "  {device_short}  promotions:{}  fallbacks:{}  attempts:{}  grant drops:{}",
            record.promotions, fallbacks, record.attempts, record.grant_drops
        ));
        if let Some(latest) = &record.latest {
            let age_ms = latest_ms
                .unwrap_or(latest.ts)
                .saturating_sub(latest.ts)
                .max(0);
            let worker = latest
                .worker_fp
                .as_deref()
                .map(|worker| worker.chars().take(12).collect::<String>())
                .unwrap_or_else(|| "-".to_string());
            let stuck = if is_stuck(record, latest_ms) {
                format!("  STUCK ({})", latest.hold.as_deref().unwrap_or("unknown"))
            } else {
                String::new()
            };
            lines.push(format!(
                "    last: {}  worker:{}  age:{}m{}",
                latest.evt,
                worker,
                age_ms / 60_000,
                stuck
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{anomaly_count, is_carrier_signal, is_stuck, record, render_section};
    use crate::doctor::digest::{Digest, classify};
    use serde_json::json;
    use std::collections::BTreeMap;

    const BASE: i64 = 1_000_000;

    fn carrier(evt: &str, ts: i64, fields: serde_json::Value) -> serde_json::Value {
        json!({"ts": ts, "level":"warn", "target": "signal", "src": "spa", "device": "0123456789abcdef", "evt": evt, "kv": fields.to_string()})
    }

    #[test]
    fn carrier_lines_leave_generic_signals_but_non_carrier_spa_signals_remain() {
        let carrier_line = carrier(
            "direct route fell back",
            BASE,
            json!({"target":"carriers", "reason":"timeout"}),
        );
        assert!(is_carrier_signal(&carrier_line));
        let mut digest = Digest::new();
        classify(&mut digest, &carrier_line, 0, "coord");
        assert!(digest.signals.is_empty());
        assert_eq!(digest.browser_carriers.len(), 1);

        let ordinary = json!({"ts": BASE, "level":"warn", "target":"signal", "src":"spa", "evt":"spa.uncaught", "kv":"{\"target\":\"terminal\"}"});
        assert!(!is_carrier_signal(&ordinary));
        classify(&mut digest, &ordinary, 0, "coord");
        assert_eq!(digest.signals.get("spa.uncaught").unwrap().count, 1);
    }

    #[test]
    fn fallback_counts_are_grouped_by_reason() {
        let mut records = BTreeMap::new();
        record(
            &mut records,
            &carrier(
                "direct route fell back",
                BASE,
                json!({"target":"carriers", "reason":"timeout"}),
            ),
        );
        record(
            &mut records,
            &carrier(
                "direct route fell back",
                BASE + 1,
                json!({"target":"carriers", "reason":"timeout"}),
            ),
        );
        record(
            &mut records,
            &carrier(
                "direct route fell back",
                BASE + 2,
                json!({"target":"carriers", "reason":"closed"}),
            ),
        );
        assert_eq!(records.values().next().unwrap().fallbacks["timeout"], 2);
        assert_eq!(records.values().next().unwrap().fallbacks["closed"], 1);
    }

    #[test]
    fn stuck_boundary_and_later_promotion_are_folded_from_latest_event() {
        let mut records = BTreeMap::new();
        let held = carrier(
            "direct peer attempt held",
            BASE,
            json!({"target":"carriers", "hold":"hold_down"}),
        );
        record(&mut records, &held);
        let summary = records.values().next().unwrap();
        assert!(!is_stuck(summary, Some(BASE + 4 * 60_000 + 59_000)));
        assert!(is_stuck(summary, Some(BASE + 5 * 60_000)));
        record(
            &mut records,
            &carrier(
                "direct route promoted",
                BASE + 5 * 60_000,
                json!({"target":"carriers"}),
            ),
        );
        assert!(!is_stuck(
            records.values().next().unwrap(),
            Some(BASE + 10 * 60_000)
        ));
        assert_eq!(anomaly_count(&records, Some(BASE + 10 * 60_000)), 0);
    }

    #[test]
    fn carrier_section_render_includes_counts_latest_age_and_stuck_hold() {
        let mut records = BTreeMap::new();
        record(
            &mut records,
            &carrier("direct route promoted", BASE, json!({"target":"carriers"})),
        );
        record(
            &mut records,
            &carrier(
                "direct route fell back",
                BASE + 1,
                json!({"target":"carriers", "reason":"timeout"}),
            ),
        );
        record(
            &mut records,
            &carrier(
                "direct peer attempt started",
                BASE + 2,
                json!({"target":"carriers"}),
            ),
        );
        record(
            &mut records,
            &carrier(
                "direct grant dropped; a fresh mint is asked for any demand",
                BASE + 3,
                json!({"target":"carriers"}),
            ),
        );
        let line = carrier(
            "direct peer attempt held",
            BASE + 4,
            json!({"target":"carriers", "hold":"awaiting_grant", "worker_fp":"abcdef0123456789"}),
        );
        record(&mut records, &line);
        assert_eq!(
            render_section(&records, Some(BASE + 5 * 60_000 + 4)),
            [
                "## Browser direct routes",
                "  0123456789ab  promotions:1  fallbacks:timeout×1  attempts:1  grant drops:1",
                "    last: direct peer attempt held  worker:abcdef012345  age:5m  STUCK (awaiting_grant)"
            ]
        );
    }
    #[test]
    fn promotions_do_not_trip_exit_but_fallbacks_do() {
        use crate::doctor::audit::AuditSummary;
        use crate::doctor::digest_render::render_digest;

        let mut digest = Digest::new();
        classify(
            &mut digest,
            &carrier("direct route promoted", BASE, json!({"target":"carriers"})),
            0,
            "coord",
        );
        assert_eq!(
            render_digest(&digest, "24h", 0, &[], &AuditSummary::default(), "test").exit_code,
            0
        );
        classify(
            &mut digest,
            &carrier(
                "direct route fell back",
                BASE + 1,
                json!({"target":"carriers", "reason":"timeout"}),
            ),
            0,
            "coord",
        );
        assert_eq!(
            render_digest(&digest, "24h", 0, &[], &AuditSummary::default(), "test").exit_code,
            1
        );
    }
}

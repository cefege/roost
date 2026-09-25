//! Worker identity, presence, and the static host fields a fleet view renders.
//!
//! `normalize_host_identity_text` is the security boundary in this file: a host
//! field reaches the database and the browser from whatever the OS reported, so
//! it is reduced to a bounded, single-line display value with no terminal or
//! bidi controls in it. Everything past that — which machine a row belongs to —
//! is ordinary wire shape.
//!
//! The capacity report is the same shape on the worker heartbeat, the
//! coordinator's projection, and the CLI's fleet view, so it is validated here
//! where the three meet rather than in each of them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::validate::{integer_in_range, non_empty, nonnegative};
use crate::wire::brand::WorkerFp;
use crate::{ProtocolError, ProtocolResult};

pub const HOST_IDENTITY_VALUE_MAX_UTF8_BYTES: usize = 256;

/// A hostile or merely enormous host field is inspected at most this many
/// UTF-16 code units before the result is thrown away, so the normalizer's cost
/// is bounded no matter what the OS reported.
const HOST_IDENTITY_MAX_INSPECTED_CODE_UNITS: usize = 4_096;

/// The Unicode control class Cc.
fn is_control(character: char) -> bool {
    matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

/// The Unicode format class Cf, by range. A zero-width joiner or a directional
/// override from a host field is invisible in a review and reorders the label
/// beside it in every viewer.
fn is_format(character: char) -> bool {
    let code = character as u32;
    matches!(code,
        0x00ad
        | 0x0600..=0x0605
        | 0x061c
        | 0x06dd
        | 0x070f
        | 0x08e2
        | 0x180e
        | 0x200b..=0x200f
        | 0x202a..=0x202e
        | 0x2060..=0x2064
        | 0x2066..=0x206f
        | 0xfeff
        | 0xfff9..=0xfffb
        | 0x110bd
        | 0x110cd
        | 0x13430..=0x1343f
        | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a
        | 0xe0001
        | 0xe0020..=0xe007f)
}

/// The set a JavaScript `/\s/u` matches, which is not quite Rust's
/// `char::is_whitespace`: that one admits U+0085 and refuses the byte-order
/// mark, and this field is compared against what every other Roost client does
/// with the same text.
fn is_whitespace(character: char) -> bool {
    matches!(character,
        '\u{0009}'..='\u{000d}'
        | '\u{0020}'
        | '\u{00a0}'
        | '\u{1680}'
        | '\u{2000}'..='\u{200a}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{202f}'
        | '\u{205f}'
        | '\u{3000}'
        | '\u{feff}')
}

/// Cut a string to a byte budget without splitting a character.
fn truncate_to_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// A bounded, single-line display value with no terminal or bidi controls, or
/// null when the input is not a string or reduces to nothing.
pub fn normalize_host_identity_text(value: &Value) -> Option<String> {
    let Value::String(text) = value else {
        return None;
    };
    let mut inspected = 0usize;
    let mut encoded_bytes = 0usize;
    let mut output = String::new();
    for character in text.chars() {
        if inspected >= HOST_IDENTITY_MAX_INSPECTED_CODE_UNITS {
            break;
        }
        // Costed in UTF-16 code units, because that is the unit the inspection
        // budget and the wire both count in.
        inspected += character.len_utf16();
        let kept = if is_whitespace(character) {
            ' '
        } else if is_control(character) || is_format(character) {
            continue;
        } else {
            character
        };
        let character_bytes = kept.len_utf8();
        if encoded_bytes + character_bytes > HOST_IDENTITY_VALUE_MAX_UTF8_BYTES {
            break;
        }
        encoded_bytes += character_bytes;
        output.push(kept);
    }

    // Every whitespace run is now a single space and every control is gone, so
    // collapsing the runs and dropping the edges is what the trim did. Unicode
    // NFC is not applied: it needs a normalization table this crate may not
    // take a dependency for, and nothing below depends on it.
    let mut collapsed = String::with_capacity(output.len());
    for word in output.split(' ') {
        if word.is_empty() {
            continue;
        }
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    let bounded = truncate_to_utf8_bytes(&collapsed, HOST_IDENTITY_VALUE_MAX_UTF8_BYTES);
    (!bounded.is_empty()).then_some(bounded)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIdentity {
    pub hardware_model: Option<String>,
    pub chip: Option<String>,
    pub linux_distribution: Option<String>,
}

/// Canonicalize every static host field, or null when none of them survived —
/// an identity of three nulls describes no machine and is not worth a row.
pub fn normalize_host_identity(value: &Value) -> Option<HostIdentity> {
    let Value::Object(fields) = value else {
        return None;
    };
    let identity = HostIdentity {
        hardware_model: fields
            .get("hardware_model")
            .and_then(normalize_host_identity_text),
        chip: fields.get("chip").and_then(normalize_host_identity_text),
        linux_distribution: fields
            .get("linux_distribution")
            .and_then(normalize_host_identity_text),
    };
    (identity.hardware_model.is_some()
        || identity.chip.is_some()
        || identity.linux_distribution.is_some())
    .then_some(identity)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostMetrics {
    pub cpu_pct: f64,
    pub mem_used_bytes: i64,
    pub mem_total_bytes: i64,
    pub disk_used_bytes: i64,
    pub disk_total_bytes: i64,
    pub net_rx_bps: i64,
    pub net_tx_bps: i64,
    pub sampled_at_ms: i64,
}

impl HostMetrics {
    pub fn check(&self) -> ProtocolResult<()> {
        if !(0.0..=100.0).contains(&self.cpu_pct) {
            return Err(ProtocolError::new(
                "host_metrics.cpu_pct",
                format!("must be between 0 and 100, got {}", self.cpu_pct),
            ));
        }
        for (field, value) in [
            ("host_metrics.mem_used_bytes", self.mem_used_bytes),
            ("host_metrics.mem_total_bytes", self.mem_total_bytes),
            ("host_metrics.disk_used_bytes", self.disk_used_bytes),
            ("host_metrics.disk_total_bytes", self.disk_total_bytes),
            ("host_metrics.net_rx_bps", self.net_rx_bps),
            ("host_metrics.net_tx_bps", self.net_tx_bps),
        ] {
            nonnegative(field, value)?;
        }
        integer_in_range(
            "host_metrics.sampled_at_ms",
            self.sampled_at_ms,
            1,
            i64::MAX,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerOs {
    Darwin,
    Linux,
    Win32,
}

impl WorkerOs {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Darwin => "darwin",
            Self::Linux => "linux",
            Self::Win32 => "win32",
        }
    }
}

/// How many terminal cores a worker's core factory is holding, and what it
/// refused. Content-free by design: it describes pressure, never a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCoreCapacityReport {
    pub used: u32,
    pub pending: u32,
    pub capacity: u32,
    pub estimated_reserved_bytes: u64,
    pub effective_memory_ceiling_bytes: u64,
    pub boot_rss_bytes: u64,
    pub overcommit_count: u32,
    pub refusal_count: u64,
}

impl TerminalCoreCapacityReport {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let report: TerminalCoreCapacityReport = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("terminal_core_capacity", error.to_string()))?;
        if report.overcommit_count > 1 {
            return Err(ProtocolError::new(
                "terminal_core_capacity.overcommit_count",
                "terminal core replacement overcommit exceeds one reserved slot",
            ));
        }
        let in_use = u64::from(report.used) + u64::from(report.pending);
        let allowed = u64::from(report.capacity) + u64::from(report.overcommit_count);
        if in_use > allowed {
            return Err(ProtocolError::new(
                "terminal_core_capacity.used",
                "terminal core use exceeds reported capacity without replacement reserve",
            ));
        }
        Ok(report)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Worker {
    pub fp: WorkerFp,
    pub label: String,
    pub os: WorkerOs,
    pub host_identity: Option<HostIdentity>,
    /// Null = registered before the drift badge existed.
    pub git_sha: Option<String>,
    /// Volatile; decays on disconnect.
    pub host_metrics: Option<HostMetrics>,
    pub registered_at_ms: i64,
    pub last_seen_ms: i64,
    /// Stored from the worker's advertised reachable address at register time
    /// and used to build that machine's address in a client. Not used to dial
    /// the worker: it has no inbound surface, it dials the coordinator.
    pub reachable_addr: Option<String>,
    pub keeper_runtime: Option<crate::keeper_update::KeeperRuntimeObservationV1>,
    pub terminal_core_capacity: Option<TerminalCoreCapacityReport>,
}

impl Worker {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let worker: Worker = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("worker", error.to_string()))?;
        worker.check()?;
        Ok(worker)
    }

    pub fn check(&self) -> ProtocolResult<()> {
        non_empty("worker.label", &self.label)?;
        integer_in_range(
            "worker.registered_at_ms",
            self.registered_at_ms,
            1,
            i64::MAX,
        )?;
        integer_in_range("worker.last_seen_ms", self.last_seen_ms, 1, i64::MAX)?;
        if let Some(metrics) = &self.host_metrics {
            metrics.check()?;
        }
        Ok(())
    }
}

/// Worker Sync presence. The static fields travel only in the full registered
/// record, so a heartbeat is a presence signal and nothing more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// `Registered` carries the whole worker record; a heartbeat is a presence
// signal and nothing more. Boxing the record to level the variants would cost
// an allocation on the one frame per worker that carries it.
#[allow(clippy::large_enum_variant)]
pub enum WorkerPresenceEvent {
    Registered {
        worker: Worker,
    },
    Heartbeat {
        fp: WorkerFp,
        last_seen_ms: i64,
        host_metrics: Option<HostMetrics>,
        terminal_core_capacity: Option<TerminalCoreCapacityReport>,
    },
    Removed {
        fp: WorkerFp,
    },
}

impl WorkerPresenceEvent {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let event: WorkerPresenceEvent = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("worker_presence_event", error.to_string()))?;
        match &event {
            WorkerPresenceEvent::Registered { worker } => worker.check()?,
            // A heartbeat's own timestamp is unvalidated on the wire: only the
            // presence signal matters, and the coordinator stamps its own.
            WorkerPresenceEvent::Heartbeat { host_metrics, .. } => {
                if let Some(metrics) = host_metrics {
                    metrics.check()?;
                }
            }
            WorkerPresenceEvent::Removed { .. } => {}
        }
        Ok(event)
    }
}

#[cfg(test)]
mod tests;

//! The terminal core capacity report's protobuf form, which a worker sends to a
//! coordinator deciding viewport geometry. The report itself lives in
//! `wire::worker`; this file only maps it, and runs the report's own
//! cross-field rules in both directions so an inconsistent report cannot alter
//! admission visibility by arriving from a different side of the boundary.
//!
//! The byte counters are `uint64` on both sides, so the safe-integer guard the
//! JavaScript boundary needed before narrowing a `bigint` to a `Number` has
//! nothing left to check here: the whole `u64` range is the field's bound, and a
//! value outside it cannot be expressed at all.

use roost_proto::TerminalCoreCapacityReport as PbTerminalCoreCapacityReport;

use crate::wire::worker::TerminalCoreCapacityReport;
use crate::{ProtocolError, ProtocolResult};

const REPORT: &str = "terminal_core_capacity";

/// Re-check a typed report through the one validator that owns its rules. A
/// Rust caller can assemble a report the cross-field rules would refuse, and the
/// boundary is where the original refused one.
fn checked_report(
    report: &TerminalCoreCapacityReport,
) -> ProtocolResult<TerminalCoreCapacityReport> {
    TerminalCoreCapacityReport::parse(
        serde_json::to_value(report)
            .map_err(|error| ProtocolError::new(REPORT, error.to_string()))?,
    )
}

pub fn terminal_core_capacity_report_to_proto(
    report: &TerminalCoreCapacityReport,
) -> ProtocolResult<PbTerminalCoreCapacityReport> {
    let checked = checked_report(report)?;
    Ok(PbTerminalCoreCapacityReport {
        used: checked.used,
        pending: checked.pending,
        capacity: checked.capacity,
        estimated_reserved_bytes: checked.estimated_reserved_bytes,
        effective_memory_ceiling_bytes: checked.effective_memory_ceiling_bytes,
        boot_rss_bytes: checked.boot_rss_bytes,
        overcommit_count: checked.overcommit_count,
        refusal_count: checked.refusal_count,
        ..Default::default()
    })
}

pub fn terminal_core_capacity_report_from_proto(
    report: &PbTerminalCoreCapacityReport,
) -> ProtocolResult<TerminalCoreCapacityReport> {
    checked_report(&TerminalCoreCapacityReport {
        used: report.used,
        pending: report.pending,
        capacity: report.capacity,
        estimated_reserved_bytes: report.estimated_reserved_bytes,
        effective_memory_ceiling_bytes: report.effective_memory_ceiling_bytes,
        boot_rss_bytes: report.boot_rss_bytes,
        overcommit_count: report.overcommit_count,
        refusal_count: report.refusal_count,
    })
}

//! The run's record (`report.json`: every sample, every sampler bracket, the
//! machine) and its side-by-side table (`report.md`). Called by `run`; reads
//! `stats` for the summaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::BenchError;
use crate::prepare::Prepared;
use crate::sampler::{BracketReport, Role};
use crate::scenario::Sample;
use crate::stack::StackId;
use crate::stats::{Summary, summarize};

/// Metric rows, in scenario order.
const METRICS: [&str; 12] = [
    "coord_listen_ms",
    "worker_routable_ms",
    "session_spawn_rpc_ms",
    "cold_nav_first_ms",
    "cold_nav_warm_ms",
    "echo_rtt_ms",
    "echo_reply_ms",
    "echo_paint_ms",
    "flood_plain_ms",
    "flood_styled_ms",
    "fanout_p1_ms",
    "fanout_max_ms",
];
pub const SCENARIOS: [&str; 6] = [
    "startup",
    "cold_nav",
    "echo_rtt",
    "flood_plain",
    "flood_styled",
    "fanout",
];
const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// One stack's round.
#[derive(Debug, Serialize)]
pub struct RoundRecord {
    pub stack: StackId,
    pub round: u32,
    pub samples: Vec<Sample>,
    pub usage: Vec<BracketReport>,
    pub idle_rss_bytes: BTreeMap<Role, u64>,
    pub errors: BTreeMap<String, String>,
}

impl RoundRecord {
    pub fn new(stack: StackId, round: u32) -> Self {
        Self {
            stack,
            round,
            samples: Vec::new(),
            usage: Vec::new(),
            idle_rss_bytes: BTreeMap::new(),
            errors: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MachineInfo {
    pub nproc: usize,
    pub loadavg_at_start: String,
    pub kernel: String,
}

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub run_id: String,
    pub command_line: String,
    pub prepared: Prepared,
    pub machine: MachineInfo,
    pub rounds: Vec<RoundRecord>,
}

/// Where the two report files landed, and the table.
#[derive(Debug)]
pub struct WrittenReport {
    pub markdown_path: PathBuf,
    pub markdown: String,
}

pub fn write_report(run_dir: &Path, report: &RunReport) -> Result<WrittenReport, BenchError> {
    let json = serde_json::to_string_pretty(report).map_err(|error| BenchError::Decode {
        context: "encoding report.json".into(),
        detail: error.to_string(),
    })?;
    let json_path = run_dir.join("report.json");
    std::fs::write(&json_path, json)
        .map_err(|error| BenchError::io(format!("writing {}", json_path.display()), error))?;
    let markdown = render_markdown(report);
    let markdown_path = run_dir.join("report.md");
    std::fs::write(&markdown_path, &markdown)
        .map_err(|error| BenchError::io(format!("writing {}", markdown_path.display()), error))?;
    Ok(WrittenReport {
        markdown_path,
        markdown,
    })
}

/// `v3 / v2` as a multiplier; below 1 means v3 is faster or smaller.
pub fn ratio(v3: f64, v2: f64) -> String {
    if v2 > 0.0 && v3.is_finite() {
        format!("{:.2}×", v3 / v2)
    } else {
        "—".to_string()
    }
}

fn render_markdown(report: &RunReport) -> String {
    let mut table = String::new();
    let _ = writeln!(
        table,
        "| metric | v2 p50 | v2 p95 | v3 p50 | v3 p95 | v3/v2 (p50) |\n|---|---:|---:|---:|---:|---:|"
    );
    for metric in METRICS {
        let per_stack = |stack| {
            summarize(&values_where(report, stack, |round| {
                round
                    .samples
                    .iter()
                    .filter(|sample| sample.metric == metric)
                    .map(|sample| sample.value_ms)
                    .collect()
            }))
        };
        push_row(
            &mut table,
            metric,
            per_stack(StackId::V2),
            per_stack(StackId::V3),
            1.0,
        );
    }
    for scenario in SCENARIOS {
        for role in Role::ALL {
            let usage = |stack, pick: fn(&crate::sampler::RoleUsage) -> f64| {
                summarize(&values_where(report, stack, |round| {
                    round
                        .usage
                        .iter()
                        .filter(|bracket| bracket.name == scenario)
                        .filter_map(|bracket| bracket.roles.get(&role).map(pick))
                        .collect()
                }))
            };
            let cpu_label = format!("{scenario} cpu_ms {}", role.as_str());
            let cpu_pick: fn(&crate::sampler::RoleUsage) -> f64 = |usage| usage.cpu_ms;
            push_row(
                &mut table,
                &cpu_label,
                usage(StackId::V2, cpu_pick),
                usage(StackId::V3, cpu_pick),
                1.0,
            );
            let rss_label = format!("{scenario} rss_peak_mb {}", role.as_str());
            let rss_pick: fn(&crate::sampler::RoleUsage) -> f64 =
                |usage| usage.rss_peak_bytes as f64;
            push_row(
                &mut table,
                &rss_label,
                usage(StackId::V2, rss_pick),
                usage(StackId::V3, rss_pick),
                BYTES_PER_MIB,
            );
        }
    }
    for role in Role::ALL {
        let idle = |stack| {
            summarize(&values_where(report, stack, |round| {
                round
                    .idle_rss_bytes
                    .get(&role)
                    .map(|bytes| vec![*bytes as f64])
                    .unwrap_or_default()
            }))
        };
        let label = format!("idle rss_mb {}", role.as_str());
        push_row(
            &mut table,
            &label,
            idle(StackId::V2),
            idle(StackId::V3),
            BYTES_PER_MIB,
        );
    }
    table.push('\n');
    table.push_str(&render_footer(report));
    table
}

fn values_where(
    report: &RunReport,
    stack: StackId,
    pick: impl Fn(&RoundRecord) -> Vec<f64>,
) -> Vec<f64> {
    report
        .rounds
        .iter()
        .filter(|round| round.stack == stack)
        .flat_map(pick)
        .collect()
}

fn push_row(table: &mut String, label: &str, v2: Option<Summary>, v3: Option<Summary>, scale: f64) {
    if v2.is_none() && v3.is_none() {
        return;
    }
    let cell = |summary: Option<Summary>, pick: fn(&Summary) -> f64| {
        summary.map_or_else(
            || "—".to_string(),
            |summary| format!("{:.1}", pick(&summary) / scale),
        )
    };
    let ratio_cell = match (v2, v3) {
        (Some(v2), Some(v3)) => ratio(v3.p50, v2.p50),
        _ => "—".to_string(),
    };
    let _ = writeln!(
        table,
        "| {label} | {} | {} | {} | {} | {ratio_cell} |",
        cell(v2, |summary| summary.p50),
        cell(v2, |summary| summary.p95),
        cell(v3, |summary| summary.p50),
        cell(v3, |summary| summary.p95),
    );
}

fn render_footer(report: &RunReport) -> String {
    let mut footer = String::new();
    for stack in [StackId::V2, StackId::V3] {
        let rounds: Vec<&RoundRecord> = report
            .rounds
            .iter()
            .filter(|round| round.stack == stack)
            .collect();
        if rounds.is_empty() {
            continue;
        }
        let mut carriers: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        let mut long_tasks: BTreeMap<&str, (u64, f64)> = BTreeMap::new();
        for sample in rounds.iter().flat_map(|round| round.samples.iter()) {
            if let Some(carrier) = sample.detail.get("carrier") {
                carriers
                    .entry(sample.metric)
                    .or_default()
                    .insert(carrier_text(carrier));
            }
            if let Some(tasks) = sample.detail.get("longTasks") {
                let entry = long_tasks.entry(sample.metric).or_insert((0, 0.0));
                entry.0 += tasks.get("count").and_then(Value::as_u64).unwrap_or(0);
                entry.1 += tasks.get("ms").and_then(Value::as_f64).unwrap_or(0.0);
            }
        }
        let _ = writeln!(footer, "**{}** carrier per metric:", stack.as_str());
        for (metric, seen) in carriers {
            let seen: Vec<&str> = seen.iter().map(String::as_str).collect();
            let _ = writeln!(footer, "- {metric}: {}", seen.join(", "));
        }
        for (metric, (count, ms)) in long_tasks {
            let _ = writeln!(footer, "- {metric}: {count} long tasks, {ms:.0} ms total");
        }
        for round in &rounds {
            for (scenario, error) in &round.errors {
                let _ = writeln!(footer, "- round {} {scenario} FAILED: {error}", round.round);
            }
        }
        footer.push('\n');
    }
    footer
}

/// The transport the indicator named (`sync`, `loopback`, `peer`), or why none.
fn carrier_text(carrier: &Value) -> String {
    match carrier {
        Value::String(text) if text.is_empty() => "(no indicator)".to_string(),
        Value::String(text) => text.clone(),
        other => other
            .pointer("/data/terminalTransport")
            .and_then(Value::as_str)
            .map_or_else(|| other.to_string(), str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::ratio;

    #[test]
    fn ratio_is_v3_over_v2() {
        assert_eq!(ratio(0.5, 1.0), "0.50×");
        assert_eq!(ratio(3.0, 1.5), "2.00×");
    }

    #[test]
    fn ratio_without_a_v2_baseline_is_a_dash() {
        assert_eq!(ratio(1.0, 0.0), "—");
        assert_eq!(ratio(f64::NAN, 1.0), "—");
    }
}

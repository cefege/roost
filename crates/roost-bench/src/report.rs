//! The run's record (`report.json`: every sample, every sampler bracket, the
//! machine) and its table (`report.md`). Called by `run`; reads `stats` for the
//! summaries.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::BenchError;
use crate::prepare::Prepared;
use crate::sampler::{BracketReport, Role};
use crate::scenario::Sample;
use crate::stats::{Summary, summarize};

/// Metric rows, in scenario order.
const METRICS: [&str; 17] = [
    "coord_listen_ms",
    "worker_routable_ms",
    "session_spawn_rpc_ms",
    "cold_nav_first_ms",
    "cold_nav_warm_ms",
    "boot_dcl_ms",
    "boot_wasm_fetch_ms",
    "boot_wasm_to_main_ms",
    "boot_main_to_mount_ms",
    "boot_mount_to_cells_ms",
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

/// One round.
#[derive(Debug, Serialize)]
pub struct RoundRecord {
    pub round: u32,
    pub samples: Vec<Sample>,
    pub usage: Vec<BracketReport>,
    pub idle_rss_bytes: BTreeMap<Role, u64>,
    pub errors: BTreeMap<String, String>,
}

impl RoundRecord {
    pub fn new(round: u32) -> Self {
        Self {
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

fn render_markdown(report: &RunReport) -> String {
    let mut table = String::new();
    let _ = writeln!(table, "| metric | p50 | p95 |\n|---|---:|---:|");
    for metric in METRICS {
        let summary = summarize(&values_where(report, |round| {
            round
                .samples
                .iter()
                .filter(|sample| sample.metric == metric)
                .map(|sample| sample.value_ms)
                .collect()
        }));
        push_row(&mut table, metric, summary, 1.0);
    }
    for scenario in SCENARIOS {
        for role in Role::ALL {
            let usage = |pick: fn(&crate::sampler::RoleUsage) -> f64| {
                summarize(&values_where(report, |round| {
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
            push_row(&mut table, &cpu_label, usage(cpu_pick), 1.0);
            let rss_label = format!("{scenario} rss_peak_mb {}", role.as_str());
            let rss_pick: fn(&crate::sampler::RoleUsage) -> f64 =
                |usage| usage.rss_peak_bytes as f64;
            push_row(&mut table, &rss_label, usage(rss_pick), BYTES_PER_MIB);
        }
    }
    for role in Role::ALL {
        let idle = summarize(&values_where(report, |round| {
            round
                .idle_rss_bytes
                .get(&role)
                .map(|bytes| vec![*bytes as f64])
                .unwrap_or_default()
        }));
        let label = format!("idle rss_mb {}", role.as_str());
        push_row(&mut table, &label, idle, BYTES_PER_MIB);
    }
    table.push('\n');
    table.push_str(&render_footer(report));
    table
}

fn values_where(report: &RunReport, pick: impl Fn(&RoundRecord) -> Vec<f64>) -> Vec<f64> {
    report.rounds.iter().flat_map(pick).collect()
}

fn push_row(table: &mut String, label: &str, summary: Option<Summary>, scale: f64) {
    let Some(summary) = summary else {
        return;
    };
    let _ = writeln!(
        table,
        "| {label} | {:.1} | {:.1} |",
        summary.p50 / scale,
        summary.p95 / scale
    );
}

fn render_footer(report: &RunReport) -> String {
    let mut footer = String::new();
    if report.rounds.is_empty() {
        return footer;
    }
    let mut carriers: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut long_tasks: BTreeMap<&str, (u64, f64)> = BTreeMap::new();
    for sample in report.rounds.iter().flat_map(|round| round.samples.iter()) {
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
    let _ = writeln!(footer, "**carrier per metric:**");
    for (metric, seen) in carriers {
        let seen: Vec<&str> = seen.iter().map(String::as_str).collect();
        let _ = writeln!(footer, "- {metric}: {}", seen.join(", "));
    }
    for (metric, (count, ms)) in long_tasks {
        let _ = writeln!(footer, "- {metric}: {count} long tasks, {ms:.0} ms total");
    }
    for round in &report.rounds {
        for (scenario, error) in &round.errors {
            let _ = writeln!(footer, "- round {} {scenario} FAILED: {error}", round.round);
        }
    }
    footer.push('\n');
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
    use super::push_row;
    use crate::stats::summarize;

    #[test]
    fn a_row_prints_p50_and_p95_and_an_empty_row_is_omitted() {
        let mut table = String::new();
        push_row(&mut table, "echo_rtt_ms", summarize(&[2.0, 4.0]), 1.0);
        push_row(&mut table, "missing_ms", None, 1.0);
        assert_eq!(table, "| echo_rtt_ms | 2.0 | 4.0 |\n");
    }
}

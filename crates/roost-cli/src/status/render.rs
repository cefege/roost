//! The exact human-facing `roost status` line ordering, and nothing else.
//! Called by status/mod.rs (which prints the result) and by
//! tests/status_output_shape.rs, which pins the string. Depends on
//! status/report.rs for the shapes and status/update_state.rs for the one
//! classifier, and on nothing that touches the disk or a socket: the documented
//! output is a pure function of a report plus the two service labels, so it
//! can be asserted with no services running.
//!
//! The three `→` remedy blocks that named a v2 build command were re-pointed
//! at v3 commands (`roost quickstart`, `roost deploy localhost`, `dx build`),
//! because the v2 text names files a v3 install does not have. The line
//! shapes, the marks, the field order and the spacing are unchanged — see
//! docs/phase6-cli-contract.md, "status".

use crate::status::report::{SpaStatus, StatusReport, WorkerStatus};
use crate::status::update_state::{WorkerUpdateInputs, worker_update_label, worker_update_state};

/// The two service identities this host's install uses. They are parameters
/// rather than module constants because `roost-host` resolves them from the
/// environment (`ROOST_COORD_LABEL`, `ROOST_WORKER_AGENT_LABEL`) and the
/// platform, and a renderer that read the environment itself could not be
/// called twice in a test with two different installs.
#[derive(Debug, Clone, Copy)]
pub struct ServiceLabels<'a> {
    pub coord: &'a str,
    pub worker: &'a str,
}

/// The two marks every check uses. A `✗` is a failure the operator must act
/// on; the `-` on an unprobed line is deliberately a third thing, because "we
/// could not ask" must not read as "we asked and it failed".
fn mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

/// `Math.round(ms / 1000)` in the TypeScript this replaces, kept as float
/// rounding rather than integer division so a 1_500 ms age prints `2s` and not
/// `1s`. An age that rounds down reads as a fresher machine than it is.
fn seconds(millis: i64) -> i64 {
    (millis as f64 / 1000.0).round() as i64
}

/// Where one machine sits relative to the fleet's release: its short SHA plus
/// the shared update label, so the operator sees at a glance which machines are
/// behind and that they are queued to catch up.
fn worker_update_position(worker: &WorkerStatus, coord_git_sha: Option<&str>) -> String {
    // The CLI cannot see the coordinator's in-memory deploy jobs, so a deploy
    // already in flight reads as "update available" in this readout. Printing
    // "Updating…" from the worker's own age would claim a rollout this process
    // cannot see.
    let state = worker_update_state(WorkerUpdateInputs {
        worker_git_sha: worker.git_sha.as_deref(),
        coord_git_sha,
        online: !worker.stale,
        deploy_in_flight: false,
    });
    let short_sha = worker
        .git_sha
        .as_deref()
        .filter(|sha| !sha.is_empty())
        .map(|sha| format!(" · {}", &sha[..sha.len().min(8)]))
        .unwrap_or_default();
    format!("{short_sha} · {}", worker_update_label(state))
}

/// A 404 root has three distinguishable causes, and the remedy differs: the
/// stamped dist is gone, it is present but unused, or none was ever declared.
fn spa_missing_reason(spa: &SpaStatus) -> String {
    let Some(path) = spa.web_dist_path.as_deref() else {
        return "no ROOST_WEB_DIST_PATH and no embedded build".to_string();
    };
    if spa.web_dist_present {
        format!("ROOST_WEB_DIST_PATH={path} exists but the coordinator serves no page")
    } else {
        format!("ROOST_WEB_DIST_PATH={path} has no index.html")
    }
}

fn keeper_line(worker: &WorkerStatus, now_ms: i64) -> String {
    let Some(keeper) = &worker.keeper_runtime else {
        return "      keeper: update admission unproven".to_string();
    };
    let digest: String = keeper.binding_digest.chars().take(12).collect();
    let reconciled = seconds((now_ms - keeper.reconciled_at_ms).max(0));
    format!(
        "      keeper: pid {}, epoch {}, {} channel(s), bindings {digest}, reconciled {reconciled}s ago",
        keeper.keeper_pid, keeper.keeper_epoch, keeper.channel_count
    )
}

fn terminal_core_line(worker: &WorkerStatus) -> String {
    let Some(capacity) = &worker.terminal_core_capacity else {
        return "      terminal cores: capacity unavailable".to_string();
    };
    let mib = (capacity.estimated_reserved_bytes as f64 / (1024.0 * 1024.0)).round() as i64;
    format!(
        "      terminal cores: {}/{} resident, {} pending, {mib} MiB reserved, {} refused",
        capacity.used, capacity.capacity, capacity.pending, capacity.refusal_count
    )
}

/// The whole readout as one `\n`-separated string with no trailing newline.
/// `now_ms` is a parameter rather than a clock read because the same age
/// arithmetic appears in two lines and a test that pinned them from two
/// different instants would be pinning two different things.
pub fn render_status_report(
    report: &StatusReport,
    labels: ServiceLabels<'_>,
    now_ms: i64,
) -> String {
    let mut out = String::new();
    let mut push = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };

    push("roost status".to_string());
    push(format!(
        "  {} coordinator service ({})",
        mark(report.coord_agent_loaded),
        labels.coord
    ));
    if !report.coord_agent_loaded {
        push("      → roost quickstart".to_string());
    }
    push(format!(
        "  {} worker service ({})",
        mark(report.worker_agent_loaded),
        labels.worker
    ));
    if !report.worker_agent_loaded {
        push("      → roost deploy localhost".to_string());
    }

    let coord_sha = report
        .coord
        .git_sha
        .as_deref()
        .filter(|sha| !sha.is_empty());
    let coord_note = coord_sha
        .map(|sha| format!(" (git {})", &sha[..sha.len().min(8)]))
        .unwrap_or_default();
    push(format!(
        "  {} coord reachable{coord_note}",
        mark(report.coord.reachable)
    ));
    if !report.coord.reachable {
        push("      → check logs: roost logs coord".to_string());
    }

    match report.endpoint.public_url.as_deref() {
        None => push("  ✓ public url: local-only access".to_string()),
        Some(url) => {
            push(format!(
                "  {} public url {url}",
                mark(report.endpoint.answers)
            ));
            if !report.endpoint.answers {
                push(
                    "      → that URL does not answer AuthCoordIdentity; point your front door"
                        .to_string(),
                );
                push(
                    "        (Caddy, nginx, a tunnel, any reverse proxy) at the coordinator's bind"
                        .to_string(),
                );
            }
        }
    }

    match report.spa.serves {
        None => push("  - spa: not probed (no coordinator listener on this host)".to_string()),
        Some(true) => {
            let dist = report
                .spa
                .web_dist_path
                .as_deref()
                .map(|path| format!(" ({path})"))
                .unwrap_or_default();
            push(format!("  ✓ spa: served{dist}"));
        }
        Some(false) => {
            push(format!(
                "  ✗ spa: MISSING ({})",
                spa_missing_reason(&report.spa)
            ));
            push(
                "      → every page answers 404 while the API still works; build the SPA"
                    .to_string(),
            );
            push("        (dx build --release -p roost-web --platform web) and point ROOST_WEB_DIST_PATH at that dist".to_string());
        }
    }

    if report.workers.is_empty() {
        push("  ✗ workers: none registered".to_string());
    } else {
        push(format!("  workers ({}):", report.workers.len()));
        for worker in &report.workers {
            let stale_note = if worker.stale { " (STALE)" } else { "" };
            push(format!(
                "    {} {} — last seen {}s ago{stale_note}{}",
                mark(!worker.stale),
                worker.label,
                seconds(worker.age_ms),
                worker_update_position(worker, coord_sha)
            ));
            push(keeper_line(worker, now_ms));
            push(terminal_core_line(worker));
        }
    }

    if let Some(url) = report.endpoint.public_url.as_deref() {
        push(format!("  open: {url}"));
    }
    out.pop();
    out
}

/// An unconfigured front door is a valid same-origin install, so only a
/// declared-and-silent one fails the gate. The fleet rows are deliberately
/// absent from it: a sleeping laptop is deferred, not broken, and gating the
/// whole readout on one would make `roost status` red on a healthy install
/// every night. GETTING_STARTED.md says the same in prose — inspect the rows
/// rather than treating the exit status as proof the fleet converged.
pub fn status_report_is_healthy(report: &StatusReport) -> bool {
    report.coord_agent_loaded
        && report.worker_agent_loaded
        && report.coord.reachable
        && (report.endpoint.public_url.is_none() || report.endpoint.answers)
}

// This module owns the exact human-facing `roost status` line ordering.
// The command entry and quickstart share it so remedies and health gating
// stay aligned with the report fields without duplicating output decisions.

import { WORKER_UPDATE_LABELS, workerUpdateState } from "@roost/shared/fleet-update";
import { STATUS_COORD_LABEL, STATUS_WORKER_LABEL } from "./status-native-probes.ts";
import type { SpaStatus, StatusReport, WorkerStatus } from "./status-types.ts";

function mark(ok: boolean): string {
  return ok ? "✓" : "✗";
}

/** Where one machine sits relative to the fleet's release: its short SHA plus
 *  the shared update label, so the operator sees at a glance which machines are
 *  behind and that they are queued to catch up. */
function workerUpdatePosition(worker: WorkerStatus, coordGitSha: string | null): string {
  const state = workerUpdateState({
    workerGitSha: worker.gitSha,
    coordGitSha,
    deployInFlight: worker.updateOperation !== undefined
      && worker.updateOperation !== null
      && !["blocked", "failed", "succeeded"].includes(worker.updateOperation.status),
    online: !worker.stale,
  });
  const shortSha = worker.gitSha ? ` · ${worker.gitSha.slice(0, 8)}` : "";
  return `${shortSha} · ${WORKER_UPDATE_LABELS[state]}`;
}

/** A 404 root has three distinguishable causes, and the remedy differs: the
 *  stamped dist is gone, it is present but unused, or none was ever declared. */
function spaMissingReason(spa: SpaStatus): string {
  if (!spa.webDistPath) return "no ROOST_WEB_DIST_PATH and no embedded build";
  return spa.webDistPresent
    ? `ROOST_WEB_DIST_PATH=${spa.webDistPath} exists but the coordinator serves no page`
    : `ROOST_WEB_DIST_PATH=${spa.webDistPath} has no index.html`;
}

/** Print the report as ✓/✗ lines, each failing line followed by its remedy. */
export function printStatusReport(r: StatusReport): void {
  console.log("roost status");

  console.log(`  ${mark(r.coordAgentLoaded)} coordinator service (${STATUS_COORD_LABEL})`);
  if (!r.coordAgentLoaded) console.log(`      → bash apps/coord/scripts/install.sh install`);

  console.log(`  ${mark(r.workerAgentLoaded)} worker service (${STATUS_WORKER_LABEL})`);
  if (!r.workerAgentLoaded) console.log(`      → bun apps/roost-cli/src/main.ts deploy localhost`);

  console.log(`  ${mark(r.coord.reachable)} coord reachable${r.coord.gitSha ? ` (git ${r.coord.gitSha.slice(0, 8)})` : ""}`);
  if (!r.coord.reachable) console.log(`      → check logs: bun apps/roost-cli/src/main.ts logs coord`);

  if (!r.endpoint.publicUrl) {
    console.log("  - public url: not configured");
    console.log("      → set ROOST_WEB_PUBLIC_URL to the HTTPS URL your front door serves");
  } else {
    console.log(`  ${mark(r.endpoint.answers)} public url ${r.endpoint.publicUrl}`);
    if (!r.endpoint.answers) {
      console.log("      → that URL does not answer AuthCoordIdentity; point your front door");
      console.log("        (Caddy, nginx, a tunnel, any reverse proxy) at the coordinator's bind");
    }
  }

  if (r.spa.serves === null) {
    console.log("  - spa: not probed (no coordinator listener on this host)");
  } else if (r.spa.serves) {
    console.log(`  ✓ spa: served${r.spa.webDistPath ? ` (${r.spa.webDistPath})` : ""}`);
  } else {
    console.log(`  ✗ spa: MISSING (${spaMissingReason(r.spa)})`);
    console.log("      → every page answers 404 while the API still works; build the SPA");
    console.log("        (bun run --cwd apps/web build) and point ROOST_WEB_DIST_PATH at that dist");
  }

  if (r.workers.length === 0) {
    console.log(`  ✗ workers: none registered`);
  } else {
    console.log(`  workers (${r.workers.length}):`);
    for (const w of r.workers) {
      const age = Math.round(w.ageMs / 1000);
      console.log(
        `    ${mark(!w.stale)} ${w.label} — last seen ${age}s ago${w.stale ? " (STALE)" : ""}`
        + workerUpdatePosition(w, r.coord.gitSha),
      );
      const keeper = w.keeperRuntime;
      console.log(keeper
        ? `      keeper: pid ${keeper.keeper_pid}, epoch ${keeper.keeper_epoch}, ` +
          `${keeper.channel_count} channel(s), bindings ${keeper.binding_digest.slice(0, 12)}, ` +
          `reconciled ${Math.max(0, Math.round((Date.now() - keeper.reconciled_at_ms) / 1000))}s ago`
        : "      keeper: update admission unproven");
      const terminalCoreCapacity = w.terminalCoreCapacity ?? null;
      console.log(terminalCoreCapacity
        ? `      terminal cores: ${terminalCoreCapacity.used}/${terminalCoreCapacity.capacity} resident, ` +
          `${terminalCoreCapacity.pending} pending, ` +
          `${Math.round(terminalCoreCapacity.estimated_reserved_bytes / (1024 * 1024))} MiB reserved, ` +
          `${terminalCoreCapacity.refusal_count} refused`
        : "      terminal cores: capacity unavailable");
    }
  }

  if (r.endpoint.publicUrl) console.log(`  open: ${r.endpoint.publicUrl}`);
}

export function statusReportIsHealthy(report: StatusReport): boolean {
  // An unconfigured front door is a valid same-origin install, so only a
  // declared-and-silent one fails the gate.
  return report.coordAgentLoaded
    && report.workerAgentLoaded
    && report.coord.reachable
    && (report.endpoint.publicUrl === null || report.endpoint.answers);
}

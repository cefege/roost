// This module owns the exact human-facing `roost status` line ordering.
// The command entry and quickstart share it so remedies and health gating
// stay aligned with the report fields without duplicating output decisions.

import { STATUS_COORD_LABEL, STATUS_WORKER_LABEL } from "./status-native-probes.ts";
import type { StatusReport } from "./status-types.ts";

function mark(ok: boolean): string {
  return ok ? "✓" : "✗";
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

  if (r.workers.length === 0) {
    console.log(`  ✗ workers: none registered`);
  } else {
    console.log(`  workers (${r.workers.length}):`);
    for (const w of r.workers) {
      const age = Math.round(w.ageMs / 1000);
      console.log(`    ${mark(!w.stale)} ${w.label} — last seen ${age}s ago${w.stale ? " (STALE)" : ""}`);
      const keeper = w.keeperRuntime;
      console.log(keeper
        ? `      keeper: pid ${keeper.keeper_pid}, epoch ${keeper.keeper_epoch}, ` +
          `${keeper.channel_count} channel(s), bindings ${keeper.binding_digest.slice(0, 12)}, ` +
          `reconciled ${Math.max(0, Math.round((Date.now() - keeper.reconciled_at_ms) / 1000))}s ago`
        : "      keeper: update admission unproven");
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

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

  if (r.tailscale.required) {
    const tsOk = r.tailscale.running;
    console.log(`  ${mark(tsOk)} tailscale: ${r.tailscale.state}${r.tailscale.fqdn ? ` (${r.tailscale.fqdn})` : ""}`);
    if (!tsOk) {
      const darwin = process.platform === "darwin";
      if (r.tailscale.state === "NotInstalled") {
        console.log(darwin
          ? `      → install: brew install tailscale (or the Mac App Store app)`
          : `      → install: sudo dnf install -y tailscale (see https://tailscale.com/download/linux)`);
      } else {
        console.log(darwin
          ? `      → start it: tailscale up  (then approve the network extension in System Settings)`
          : `      → start it: sudo systemctl enable --now tailscaled && sudo tailscale up`);
      }
    }
  }

  console.log(`  ${mark(r.coordAgentLoaded)} coordinator service (${STATUS_COORD_LABEL})`);
  if (!r.coordAgentLoaded) console.log(`      → bash apps/coord/scripts/install.sh install`);

  console.log(`  ${mark(r.workerAgentLoaded)} worker service (${STATUS_WORKER_LABEL})`);
  if (!r.workerAgentLoaded) console.log(`      → bun apps/roost-cli/src/main.ts deploy localhost`);

  // A SOURCE handoff at COMMITTED means the coordinator legitimately moved off
  // this box — what handlers-auth.ts reports to the SPA as relocatedToUrl. Its
  // local absence is then the expected end state, not a fault. Suppressed only
  // when the coord is gone *because* of that; every other failure is untouched.
  const relocated = r.handoff?.role === "SOURCE" && r.handoff.phase === "COMMITTED";
  if (!(relocated && !r.coord.reachable)) {
    console.log(`  ${mark(r.coord.reachable)} coord reachable${r.coord.gitSha ? ` (git ${r.coord.gitSha.slice(0, 8)})` : ""}`);
    if (!r.coord.reachable) console.log(`      → check logs: bun apps/roost-cli/src/main.ts logs coord`);
  }

  if (r.handoff) {
    console.log(`  coordinator move ${r.handoff.phase} (${r.handoff.role}, → ${r.handoff.targetUrl})`);
  }

  if (r.tlsMode === "tailscale-serve") {
    console.log("  ✓ coord TLS: tailscale serve");
  } else if (r.tlsMode === "direct") {
    console.log("  ✓ coord TLS: direct certificate");
  } else {
    console.log("  ✗ coord TLS: missing");
    console.log(r.tailscale.required
      ? "      → reinstall coord to restore Tailscale Serve"
      : "      → configure both direct TLS paths");
  }

  if (r.workers.length === 0) {
    console.log(`  ✗ workers: none registered`);
  } else {
    console.log(`  workers (${r.workers.length}):`);
    for (const w of r.workers) {
      const age = Math.round(w.ageMs / 1000);
      console.log(`    ${mark(!w.stale)} ${w.label} — last seen ${age}s ago${w.stale ? " (STALE)" : ""}`);
    }
  }

  if (r.url) console.log(`  open: ${r.url}`);
}

export function statusReportIsHealthy(report: StatusReport): boolean {
  const relocatedAway = report.handoff?.role === "SOURCE" && report.handoff.phase === "COMMITTED";
  return (!report.tailscale.required || report.tailscale.running)
    && report.coordAgentLoaded
    && report.workerAgentLoaded
    && (report.coord.reachable || relocatedAway);
}

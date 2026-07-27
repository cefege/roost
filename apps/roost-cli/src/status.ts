// `roost status` / `roost doctor` — one-shot health readout for the local
// Roost install. Replaces "read launchctl + tail the logs" (the thing that
// used to need an agent) with ✓/✗ lines, each carrying its own remedy.
// Checks: Tailscale gate, both LaunchAgents, coord liveness (MiscHealth),
// worker inventory (coord DB read-only), TLS posture (coord plist).
// `statusReport()` is also called by quickstart.ts for its final readout.

import { Database } from "bun:sqlite";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { log } from "@roost/shared/log";
import { coordDataDir, coordServicePath } from "@roost/shared/paths";
import { COORD_UNIT, WORKER_UNIT } from "./service-ctl.ts";

const COORD_LABEL = "com.roost.coordinator-v2";
const WORKER_LABEL = "com.roost.worker-v2";
const COORD_DATA_DIR = coordDataDir();
const COORD_DB = join(COORD_DATA_DIR, "coordinator_v2.db");
const COORD_PLIST = coordServicePath();
const WORKER_STALE_MS = 90_000;
// Default of cfg.handoffPath (apps/shared/src/config.ts:71); the CLI does not
// load the coord config, so honour the same env override by hand.
const COORD_HANDOFF = process.env.ROOST_COORDINATOR_HANDOFF_PATH ?? join(COORD_DATA_DIR, "coord-handoff.json");

interface WorkerStatus {
  label: string;
  lastSeenMs: number;
  ageMs: number;
  stale: boolean;
}

/** Coordinator-move state as the coord persists it (coord-move/state.ts).
 *  Structural on purpose — roost-cli imports @roost/shared only, never coord. */
export interface HandoffStatus {
  role: "SOURCE" | "TARGET";
  phase: string;
  handoffId: string;
  sourceUrl: string;
  targetUrl: string;
}

export interface StatusReport {
  tailscale: { state: string; fqdn: string | null; running: boolean };
  coordAgentLoaded: boolean;
  workerAgentLoaded: boolean;
  coord: { reachable: boolean; gitSha: string | null };
  workers: WorkerStatus[];
  tlsCertConfigured: boolean;
  url: string | null;
  handoff: HandoffStatus | null;
}

function runCapture(cmd: string[]): { exit: number; stdout: string } {
  try {
    const r = Bun.spawnSync(cmd);
    return { exit: r.exitCode ?? 1, stdout: r.stdout.toString() };
  } catch {
    return { exit: 127, stdout: "" };
  }
}

/** Tailscale BackendState + own tailnet FQDN. state="NotInstalled" when the
 *  binary is missing. fqdn is the trailing-dot-stripped Self.DNSName.
 *  Exported — quickstart.ts uses it for the hard Tailscale gate. */
export function resolveTailscale(): { state: string; fqdn: string | null } {
  const r = runCapture(["tailscale", "status", "--json"]);
  if (r.exit === 127) return { state: "NotInstalled", fqdn: null };
  if (r.exit !== 0 || !r.stdout) return { state: "Stopped", fqdn: null };
  try {
    const j = JSON.parse(r.stdout) as { BackendState?: string; Self?: { DNSName?: string } };
    const fqdn = (j.Self?.DNSName ?? "").replace(/\.$/, "") || null;
    return { state: j.BackendState ?? "Unknown", fqdn };
  } catch {
    return { state: "Unknown", fqdn: null };
  }
}

export interface TailscalePreflightDeps {
  resolve: () => { state: string; fqdn: string | null };
  log: (msg: string) => void;
  sleep: (ms: number) => Promise<void>;
  now: () => number;
  brewInstall?: () => void | Promise<void>;
}

/** Interactive Tailscale gate: loop until Tailscale is Running with a tailnet
 *  FQDN, guiding the user through the (sudo, un-scriptable) steps and POLLING
 *  system state — no stdin, so it works under `curl … | bash`. The Homebrew
 *  open-source tailscaled needs NO System Settings network-extension approval
 *  (that's only the GUI App Store/standalone app), so `tailscale cert`/`serve`
 *  are reachable non-interactively. Guidance prints once; throws on timeout so
 *  the caller dies with the same remedy. Pure/injectable for tests. */
export async function ensureTailscale(
  deps: TailscalePreflightDeps,
  timeoutMs = 180_000,
  pollMs = 1500,
): Promise<{ state: string; fqdn: string }> {
  const deadline = deps.now() + timeoutMs;
  let guided = false;
  for (;;) {
    const ts = deps.resolve();
    if (ts.state === "Running" && ts.fqdn) return { state: ts.state, fqdn: ts.fqdn };
    if (!guided) {
      if (ts.state === "NotInstalled") {
        deps.log("Tailscale is required and not installed. Installing the open-source");
        deps.log("CLI daemon (no System Settings approval needed):");
        deps.log("  brew install tailscale");
        await deps.brewInstall?.();
        deps.log("Then start it (needs sudo — run this yourself):");
        deps.log("  sudo tailscaled install-system-daemon && sudo tailscale up");
      } else {
        deps.log(`Tailscale is installed but not running (state: ${ts.state}). Start it:`);
        deps.log("  sudo tailscaled install-system-daemon && sudo tailscale up");
      }
      deps.log("Waiting for Tailscale to come up… (Ctrl-C to abort)");
      guided = true;
    }
    if (deps.now() >= deadline) {
      throw new Error(
        `Tailscale did not come up within ${Math.round(timeoutMs / 1000)}s. ` +
          "Run the commands above, then re-run: roost quickstart",
      );
    }
    await deps.sleep(pollMs);
  }
}

/** Tailnet DNS suffix (e.g. "tailXXXXXX.ts.net") for building peer FQDNs.
 *  ROOST_TAILNET_SUFFIX ?? first-label-stripped tailscale Self.DNSName ?? null.
 *  null → caller must fail loud; never fall back to a personal literal.
 *  Consumers: deploy.ts (reachable_addr), push.ts is targets not suffix. */
export function tailnetSuffix(): string | null {
  if (process.env.ROOST_TAILNET_SUFFIX) return process.env.ROOST_TAILNET_SUFFIX;
  const fqdn = resolveTailscale().fqdn;
  return fqdn ? fqdn.split(".").slice(1).join(".").replace(/\.$/, "") || null : null;
}

/** Worker/coord service loaded? launchd on macOS, systemd --user on Linux. */
function launchAgentLoaded(label: string): boolean {
  if (process.platform === "linux") {
    const unit = label === WORKER_LABEL ? WORKER_UNIT : COORD_UNIT;
    return runCapture(["systemctl", "--user", "is-active", unit]).exit === 0;
  }
  const uid = process.getuid?.() ?? "";
  return runCapture(["launchctl", "print", `gui/${uid}/${label}`]).exit === 0;
}

/** POST the public MiscHealth Connect RPC (JSON protocol). Returns null on any
 *  network/TLS/HTTP failure so the caller renders ✗ rather than throwing. */
async function coordHealth(fqdn: string | null): Promise<{ reachable: boolean; gitSha: string | null }> {
  if (!fqdn) return { reachable: false, gitSha: null };
  try {
    const res = await fetch(`https://${fqdn}:4102/roost.v1.CoordinatorService/MiscHealth`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
      signal: AbortSignal.timeout(5000),
    });
    if (!res.ok) return { reachable: false, gitSha: null };
    const j = (await res.json()) as { ok?: boolean; gitSha?: string };
    return { reachable: j.ok === true, gitSha: j.gitSha ?? null };
  } catch {
    return { reachable: false, gitSha: null };
  }
}

/** Read the coord DB read-only for the worker roster. Empty on missing DB. */
function workerInventory(): WorkerStatus[] {
  if (!existsSync(COORD_DB)) return [];
  let db: Database | null = null;
  try {
    db = new Database(COORD_DB, { readonly: true });
    const rows = db.query("SELECT label, last_seen_ms FROM workers").all() as {
      label: string;
      last_seen_ms: number;
    }[];
    const now = Date.now();
    return rows.map((r) => {
      const ageMs = now - r.last_seen_ms;
      return { label: r.label, lastSeenMs: r.last_seen_ms, ageMs, stale: ageMs > WORKER_STALE_MS };
    });
  } catch (error) {
    log.warn("status", "worker_inventory_failed", { error: String(error) });
    return [];
  } finally {
    db?.close();
  }
}

function tlsCertConfigured(): boolean {
  if (!existsSync(COORD_PLIST)) return false;
  try {
    const text = readFileSync(COORD_PLIST, "utf8");
    return /<key>ROOST_TLS_CERT_PATH<\/key>/.test(text);
  } catch {
    return false;
  }
}

/** Read coord-handoff.json (snake_case on disk). null on missing, unreadable
 *  or half-written JSON — a broken handoff file must never fail `roost status`. */
function readHandoff(): HandoffStatus | null {
  if (!existsSync(COORD_HANDOFF)) return null;
  try {
    const j = JSON.parse(readFileSync(COORD_HANDOFF, "utf8")) as Record<string, unknown>;
    const { phase, handoff_id: handoffId, source_url: sourceUrl, target_url: targetUrl } = j;
    const role = j.role === "SOURCE" ? "SOURCE" : j.role === "TARGET" ? "TARGET" : null;
    if (!role) return null;
    if (typeof phase !== "string" || typeof handoffId !== "string"
      || typeof sourceUrl !== "string" || typeof targetUrl !== "string") return null;
    return { role, phase, handoffId, sourceUrl, targetUrl };
  } catch (error) {
    log.warn("status", "handoff_read_failed", { error: String(error) });
    return null;
  }
}

export async function statusReport(): Promise<StatusReport> {
  const ts = resolveTailscale();
  const coord = await coordHealth(ts.fqdn);
  return {
    tailscale: { state: ts.state, fqdn: ts.fqdn, running: ts.state === "Running" },
    coordAgentLoaded: launchAgentLoaded(COORD_LABEL),
    workerAgentLoaded: launchAgentLoaded(WORKER_LABEL),
    coord,
    workers: workerInventory(),
    tlsCertConfigured: tlsCertConfigured(),
    url: ts.fqdn ? `https://${ts.fqdn}:4102` : null,
    handoff: readHandoff(),
  };
}

function mark(ok: boolean): string {
  return ok ? "✓" : "✗";
}

/** Print the report as ✓/✗ lines, each failing line followed by its remedy. */
export function printStatusReport(r: StatusReport): void {
  console.log("roost status");

  const tsOk = r.tailscale.running;
  console.log(`  ${mark(tsOk)} tailscale: ${r.tailscale.state}${r.tailscale.fqdn ? ` (${r.tailscale.fqdn})` : ""}`);
  if (!tsOk) {
    if (r.tailscale.state === "NotInstalled") console.log(`      → install: brew install tailscale (or the Mac App Store app)`);
    else console.log(`      → start it: tailscale up  (then approve the network extension in System Settings)`);
  }

  console.log(`  ${mark(r.coordAgentLoaded)} coordinator LaunchAgent (${COORD_LABEL})`);
  if (!r.coordAgentLoaded) console.log(`      → bash apps/coord/scripts/install.sh install`);

  console.log(`  ${mark(r.workerAgentLoaded)} worker LaunchAgent (${WORKER_LABEL})`);
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

  console.log(`  ${mark(r.tlsCertConfigured)} coord TLS cert configured`);
  if (!r.tlsCertConfigured) console.log(`      → mint: tailscale cert <fqdn> ; then reinstall coord (phones/other devices need HTTPS)`);

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

export async function status(_args: string[]): Promise<void> {
  const report = await statusReport();
  printStatusReport(report);
  // Non-zero exit when anything critical is down, so `roost status` is usable
  // as a scriptable gate (quickstart, CI, the install.sh tail).
  const relocatedAway = report.handoff?.role === "SOURCE" && report.handoff.phase === "COMMITTED";
  const healthy = report.tailscale.running && report.coordAgentLoaded
    && report.workerAgentLoaded && (report.coord.reachable || relocatedAway);
  process.exit(healthy ? 0 : 1);
}

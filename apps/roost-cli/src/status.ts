// `roost status` / `roost doctor` — one-shot health readout for the local
// Roost install. Replaces "read launchctl/systemctl + tail the logs" (the
// thing that used to need an agent) with ✓/✗ lines, each carrying its own
// remedy. Checks: Tailscale gate, both services, coord liveness
// (MiscHealth), worker inventory (coord DB read-only), TLS posture (the
// coord plist on macOS, the coord systemd unit on Linux).
// `statusReport()` is also called by quickstart.ts for its final readout.

import { Database } from "bun:sqlite";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { join, win32 } from "node:path";
import { log } from "@roost/shared/log";
import { runWindowsHelperSync, type WindowsServiceSnapshot } from "@roost/shared/windows-helper";
import { coordDataDir, coordServiceLabel, coordServicePath, workerServiceLabel } from "@roost/shared/paths";
import {
  COORD_UNIT,
  WINDOWS_SERVICE_NAMES,
  WORKER_UNIT,
  windowsServiceDefinitionsPath,
} from "./service-ctl.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";

const COORD_LABEL = coordServiceLabel();
const WORKER_LABEL = workerServiceLabel();
const WORKER_STALE_MS = 90_000;


function defaultCoordinatorDbPath(): string {
  const dataDir = process.env.ROOST_COORD_DATA_DIR ?? coordDataDir();
  return process.env.ROOST_COORDINATOR_DB
    ?? join(dataDir, "coordinator_v2.db");
}

function coordinatorServiceFile(): string {
  return process.platform === "win32"
    ? windowsServiceDefinitionsPath()
    : coordServicePath();
}

function coordinatorHandoffPath(): string {
  const dataDir = process.env.ROOST_COORD_DATA_DIR ?? coordDataDir();
  return process.env.ROOST_COORDINATOR_HANDOFF_PATH
    ?? join(dataDir, "coord-handoff.json");
}

export interface WorkerStatus {
  fingerprint: string;
  label: string;
  os: string;
  reachableAddr: string | null;
  gitSha: string | null;
  keeperState: "current" | "unknown" | "stale";
  keeperBuild: string | null;
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
  tlsMode: "tailscale-serve" | "direct" | "missing";
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
function tailscaleExecutable(): string {
  if (process.platform !== "win32") return "tailscale";
  const executable = process.env.ROOST_TAILSCALE_EXE?.trim();
  if (!executable || !win32.isAbsolute(executable) || /[\0\r\n]/.test(executable)) {
    throw new Error("Windows status requires the trusted absolute ROOST_TAILSCALE_EXE");
  }
  const info = lstatSync(executable);
  if (!info.isFile() || info.isSymbolicLink()) {
    throw new Error("ROOST_TAILSCALE_EXE must be a non-reparse regular file");
  }
  return executable;
}

export function resolveTailscale(): { state: string; fqdn: string | null } {
  const r = runCapture([tailscaleExecutable(), "status", "--json"]);
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
  /** darwin-only; Linux has no scripted install step. */
  brewInstall?: () => void | Promise<void>;
  /** Defaults to the host platform; injected so both guidance branches are testable. */
  platform?: string;
}

/** Interactive Tailscale gate: loop until Tailscale is Running with a tailnet
 *  FQDN, guiding the user through the (sudo, un-scriptable) steps and POLLING
 *  system state — no stdin, so it works under `curl … | bash`. The Homebrew
 *  open-source tailscaled needs NO System Settings network-extension approval
 *  (that's only the GUI App Store/standalone app), so `tailscale cert`/`serve`
 *  are reachable non-interactively. On Linux the distro package plus an
 *  operator grant plays the same role. Guidance prints once; throws on timeout
 *  so the caller dies with the same remedy. Pure/injectable for tests. */
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
      const darwin = (deps.platform ?? process.platform) === "darwin";
      if (ts.state === "NotInstalled") {
        if (darwin) {
          deps.log("Tailscale is required and not installed. Installing the open-source");
          deps.log("CLI daemon (no System Settings approval needed):");
          deps.log("  brew install tailscale");
          await deps.brewInstall?.();
          deps.log("Then start it (needs sudo — run this yourself):");
          deps.log("  sudo tailscaled install-system-daemon && sudo tailscale up");
        } else {
          deps.log("Tailscale is required and not installed. Install it (needs sudo —");
          deps.log("run this yourself; see https://tailscale.com/download/linux to add the repo):");
          deps.log("  sudo dnf install -y tailscale");
          deps.log("Then start it and grant yourself cert/serve access:");
          deps.log("  sudo systemctl enable --now tailscaled && sudo tailscale up");
          deps.log("  sudo tailscale set --operator=$USER");
        }
      } else if (darwin) {
        deps.log(`Tailscale is installed but not running (state: ${ts.state}). Start it:`);
        deps.log("  sudo tailscaled install-system-daemon && sudo tailscale up");
      } else {
        deps.log(`Tailscale is installed but not running (state: ${ts.state}). Start it:`);
        deps.log("  sudo systemctl enable --now tailscaled && sudo tailscale up");
        deps.log("  sudo tailscale set --operator=$USER");
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

/** Worker/coord service running under the native platform manager? */
function launchAgentLoaded(label: string): boolean {
  const worker = label === WORKER_LABEL;
  switch (process.platform) {
    case "linux":
      return runCapture([
        "systemctl",
        "--user",
        "is-active",
        worker ? WORKER_UNIT : COORD_UNIT,
      ]).exit === 0;
    case "darwin": {
      const uid = process.getuid?.() ?? "";
      return runCapture(["launchctl", "print", `gui/${uid}/${label}`]).exit === 0;
    }
    case "win32": {
      try {
        const service = runWindowsHelperSync<WindowsServiceSnapshot>(
          "service-query",
          [worker ? WINDOWS_SERVICE_NAMES.worker : WINDOWS_SERVICE_NAMES.coordinator, "basic"],
        );
        return service.state === "running";
      } catch {
        return false;
      }
    }
    default:
      throw new Error(`unsupported status platform: ${process.platform}`);
  }
}

/** POST the public MiscHealth Connect RPC (JSON protocol). Returns null on any
 *  network/TLS/HTTP failure so the caller renders ✗ rather than throwing. */
async function coordHealth(
  fqdn: string | null,
  httpsPort: string,
): Promise<{ reachable: boolean; gitSha: string | null }> {
  if (!fqdn) return { reachable: false, gitSha: null };
  try {
    const res = await fetch(`https://${fqdn}:${httpsPort}/roost.v1.CoordinatorService/MiscHealth`, {
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
export function workerInventory(databasePath: string = installedCoordinatorDbPath()): WorkerStatus[] {
  if (!existsSync(databasePath)) return [];
  let db: Database | null = null;
  try {
    db = new Database(databasePath, { readonly: true });
    const rows = db.query(
      "SELECT fp, label, os, reachable_addr, git_sha, keeper_stale, last_seen_ms FROM workers",
    ).all() as {
      fp: string;
      label: string;
      os: string;
      reachable_addr: string | null;
      git_sha: string | null;
      keeper_stale: string | null;
      last_seen_ms: number;
    }[];
    const now = Date.now();
    return rows.map((r) => {
      const ageMs = now - r.last_seen_ms;
      return {
        fingerprint: r.fp,
        label: r.label,
        os: r.os,
        reachableAddr: r.reachable_addr,
        gitSha: r.git_sha,
        keeperState: r.keeper_stale === null
          ? "unknown"
          : r.keeper_stale.length === 0 ? "current" : "stale",
        keeperBuild: r.keeper_stale && r.keeper_stale.length > 0 ? r.keeper_stale : null,
        lastSeenMs: r.last_seen_ms,
        ageMs,
        stale: ageMs > WORKER_STALE_MS,
      };
    });
  } catch (error) {
    log.warn("status", "worker_inventory_failed", { error: String(error) });
    return [];
  } finally {
    db?.close();
  }
}

function serviceEnvironmentValue(
  serviceDefinition: string,
  name: string,
  platform: NodeJS.Platform,
): string | null {
  switch (platform) {
    case "darwin":
    case "linux":
      return parsePosixServiceEnvironment(serviceDefinition, platform)[name] ?? null;
    case "win32": {
      try {
        const stored = JSON.parse(serviceDefinition) as {
          services?: { coordinator?: { environment?: Record<string, unknown> } };
        };
        const value = stored.services?.coordinator?.environment?.[name];
        return typeof value === "string" ? value : null;
      } catch {
        return null;
      }
    }
    default:
      throw new Error(`unsupported TLS service platform: ${platform}`);
  }
}

export function resolveCoordinatorDbPath(
  serviceDefinition: string | null,
  platform: NodeJS.Platform = process.platform,
  fallback: string = defaultCoordinatorDbPath(),
): string {
  if (!serviceDefinition) return fallback;
  const installed = serviceEnvironmentValue(serviceDefinition, "ROOST_COORDINATOR_DB", platform);
  return installed ? installed : fallback;
}

function installedCoordinatorDbPath(): string {
  const serviceFile = coordinatorServiceFile();
  if (!existsSync(serviceFile)) return defaultCoordinatorDbPath();
  try {
    return resolveCoordinatorDbPath(
      readFileSync(serviceFile, "utf8"),
      process.platform,
      defaultCoordinatorDbPath(),
    );
  } catch {
    return defaultCoordinatorDbPath();
  }
}

export function resolveTlsMode(
  serviceDefinition: string | null,
  tailscaleServeStatus: string | null,
  platform: NodeJS.Platform = process.platform,
): StatusReport["tlsMode"] {
  if (!serviceDefinition) return "missing";
  if (serviceEnvironmentValue(serviceDefinition, "ROOST_FRONTED", platform) === "1") {
    const loopbackPort = serviceEnvironmentValue(
      serviceDefinition,
      "ROOST_COORD_LOOPBACK_PORT",
      platform,
    ) ?? "4103";
    return tailscaleServeStatus?.includes(`http://127.0.0.1:${loopbackPort}`)
      ? "tailscale-serve"
      : "missing";
  }
  const cert = serviceEnvironmentValue(serviceDefinition, "ROOST_TLS_CERT_PATH", platform);
  const key = serviceEnvironmentValue(serviceDefinition, "ROOST_TLS_KEY_PATH", platform);
  return cert && key ? "direct" : "missing";
}

function currentTlsMode(): StatusReport["tlsMode"] {
  const serviceFile = coordinatorServiceFile();
  if (!existsSync(serviceFile)) return "missing";
  try {
    const serviceDefinition = readFileSync(serviceFile, "utf8");
    const serve = runCapture([tailscaleExecutable(), "serve", "status"]);
    return resolveTlsMode(serviceDefinition, serve.exit === 0 ? serve.stdout : null);
  } catch {
    return "missing";
  }
}

/** Read coord-handoff.json (snake_case on disk). null on missing, unreadable
 *  or half-written JSON — a broken handoff file must never fail `roost status`. */
function readHandoff(): HandoffStatus | null {
  const handoffPath = coordinatorHandoffPath();
  if (!existsSync(handoffPath)) return null;
  try {
    const j = JSON.parse(readFileSync(handoffPath, "utf8")) as Record<string, unknown>;
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
  let serviceDefinition: string | null = null;
  const serviceFile = coordinatorServiceFile();
  try {
    if (existsSync(serviceFile)) serviceDefinition = readFileSync(serviceFile, "utf8");
  } catch { /* status remains available with a damaged definition */ }
  const httpsPort = serviceDefinition
    ? serviceEnvironmentValue(serviceDefinition, "ROOST_TAILNET_HTTPS_PORT", process.platform) ?? "4102"
    : "4102";
  const coord = await coordHealth(ts.fqdn, httpsPort);
  return {
    tailscale: { state: ts.state, fqdn: ts.fqdn, running: ts.state === "Running" },
    coordAgentLoaded: launchAgentLoaded(COORD_LABEL),
    workerAgentLoaded: launchAgentLoaded(WORKER_LABEL),
    coord,
    workers: workerInventory(),
    tlsMode: currentTlsMode(),
    url: ts.fqdn ? `https://${ts.fqdn}:${httpsPort}` : null,
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

  console.log(`  ${mark(r.coordAgentLoaded)} coordinator service (${COORD_LABEL})`);
  if (!r.coordAgentLoaded) console.log(`      → bash apps/coord/scripts/install.sh install`);

  console.log(`  ${mark(r.workerAgentLoaded)} worker service (${WORKER_LABEL})`);
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
    console.log("      → reinstall coord to restore Tailscale Serve, or configure both direct TLS paths");
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

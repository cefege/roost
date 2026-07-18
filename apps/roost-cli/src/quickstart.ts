// `roost quickstart` — the local one-shot installer. Single Mac, zero agent.
// Turns the old 7-step manual install into one command: Tailscale gate →
// build SPA → mint TLS cert → install coord → deploy local worker → health →
// status → open the browser already-authorized via a self-minted ?pair token.
//
// Tailscale is REQUIRED (the install stops without it — see resolveTailscale
// gate below). Other machines/workers are NOT set up here (deferred).
// Calls: deploy() for the local worker, statusReport() for the readout.

import { spawn } from "bun";
import { Database } from "bun:sqlite";
import { existsSync, mkdirSync, writeFileSync, chmodSync } from "node:fs";
import { homedir } from "node:os";
import { join, basename } from "node:path";
import { deploy } from "./deploy.ts";
import { resolveTailscale, ensureTailscale, statusReport, printStatusReport } from "./status.ts";
import { ROOST_VERSION } from "./version.ts";
import { installCoordAgent, installWorkerAgent } from "./install-binary-agents.ts";

const COORD_DATA_DIR = join(homedir(), "Library", "Application Support", "RoostCoordinatorV2");
const COORD_DB = join(COORD_DATA_DIR, "coordinator_v2.db");
const COORD_TLS_DIR = join(COORD_DATA_DIR, "tls");
const WEB_DIST_INDEX = "apps/web/dist/index.html";

function logStep(msg: string): void {
  console.log(`>> ${msg}`);
}

function die(msg: string, ...hint: string[]): never {
  console.error(`\nERROR: ${msg}`);
  for (const h of hint) console.error(`  ${h}`);
  process.exit(1);
}

/** Run a command with inherited stdio (user sees live output). Returns exit. */
async function runInherit(cmd: string[], cwd?: string): Promise<number> {
  const proc = spawn({ cmd, cwd, stdio: ["inherit", "inherit", "inherit"] });
  await proc.exited;
  return proc.exitCode ?? 1;
}

/** Run a command capturing output (for tailscale cert). */
function runCapture(cmd: string[]): { exit: number; stdout: string; stderr: string } {
  const r = Bun.spawnSync(cmd);
  return { exit: r.exitCode ?? 1, stdout: r.stdout.toString(), stderr: r.stderr.toString() };
}

/** Insert a one-shot browser bootstrap token directly into the coord DB —
 *  same row shape as router.ts::authMintBootstrap. No RPC, no loopback
 *  bypass: quickstart runs on the coord host and owns the DB file. The host
 *  browser redeems it via the public authRedeemBrowser on first load. */
function mintBrowserToken(label: string): string {
  const rand = new Uint8Array(18);
  crypto.getRandomValues(rand);
  const token = "roost_bt_" + Array.from(rand).map((b) => b.toString(16).padStart(2, "0")).join("");
  const now = Date.now();
  const db = new Database(COORD_DB);
  try {
    db.query(
      `INSERT INTO bootstrap_tokens (token, kind, label, created_at_ms, expires_at_ms, used_at_ms, used_by_fp)
       VALUES (?, 'browser', ?, ?, ?, NULL, NULL)`,
    ).run(token, label, now, now + 24 * 60 * 60 * 1000);
  } finally {
    db.close();
  }
  return token;
}

/** Insert a one-shot WORKER bootstrap token directly into the coord DB (kind
 *  "worker" — authRedeemWorker enrolls the worker's key from it). Same owner-
 *  of-the-DB-file basis as mintBrowserToken; used by binary-mode quickstart so
 *  the local worker authorizes on a fresh install without a manual token. */
function mintWorkerToken(label: string): string {
  const rand = new Uint8Array(18);
  crypto.getRandomValues(rand);
  const token = "roost_bt_" + Array.from(rand).map((b) => b.toString(16).padStart(2, "0")).join("");
  const now = Date.now();
  const db = new Database(COORD_DB);
  try {
    db.query(
      `INSERT INTO bootstrap_tokens (token, kind, label, created_at_ms, expires_at_ms, used_at_ms, used_by_fp)
       VALUES (?, 'worker', ?, ?, ?, NULL, NULL)`,
    ).run(token, label, now, now + 24 * 60 * 60 * 1000);
  } finally {
    db.close();
  }
  return token;
}

/** Mint the tailnet TLS cert via `tailscale cert`. Skip if present unless force. */
function mintCert(fqdn: string, force: boolean): void {
  const certPath = join(COORD_TLS_DIR, `${fqdn}.crt`);
  const keyPath = join(COORD_TLS_DIR, `${fqdn}.key`);
  mkdirSync(COORD_TLS_DIR, { recursive: true });
  if (!force && existsSync(certPath) && existsSync(keyPath)) {
    logStep(`TLS cert present for ${fqdn} (skipping mint; --force to re-mint)`);
  } else {
    logStep(`minting TLS cert for ${fqdn}`);
    const cert = runCapture(["tailscale", "cert", "--cert-file", certPath, "--key-file", keyPath, fqdn]);
    if (cert.exit !== 0 && !existsSync(certPath)) {
      die(`tailscale cert failed: ${cert.stderr.trim() || cert.stdout.trim()}`,
        "HTTPS is required (browsers need a secure context off localhost).");
    }
  }
  console.log(`   cert: ${certPath}`);
}

/** Drop an `roost` shim on PATH so `roost status` / `roost logs` work from any
 *  directory (the workspace bin isn't globally linked). Targets ~/.bun/bin —
 *  the Bun installer the curl front-door uses puts it on PATH. Returns the
 *  shim path, or null if the dir isn't on PATH (caller prints a hint). */
function installRoostShim(repoDir: string): { path: string; onPath: boolean } | null {
  const binDir = join(homedir(), ".bun", "bin");
  try {
    mkdirSync(binDir, { recursive: true });
    const shim = join(binDir, "roost");
    writeFileSync(shim, `#!/usr/bin/env bash\nexec "${process.execPath}" "${join(repoDir, "apps/roost-cli/src/main.ts")}" "$@"\n`);
    chmodSync(shim, 0o755);
    const onPath = (process.env.PATH ?? "").split(":").includes(binDir);
    return { path: shim, onPath };
  } catch {
    return null;
  }
}

async function waitForCoordHealth(fqdn: string, timeoutMs = 15_000): Promise<boolean> {
  const url = `https://${fqdn}:4102/roost.v1.CoordinatorService/MiscHealth`;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: "{}",
        signal: AbortSignal.timeout(3000),
      });
      if (res.ok) return true;
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 750));
  }
  return false;
}

export async function quickstart(args: string[]): Promise<void> {
  const force = args.includes("--force");
  const dry = args.includes("--dry-run"); // generate plists only; no cert/launchctl/browser
  const binary = basename(process.execPath) !== "bun"; // compiled `roost` vs `bun … main.ts`

  // 1. Tailscale gate (interactive; skipped for --dry-run).
  let fqdn: string;
  if (dry) {
    fqdn = resolveTailscale().fqdn ?? "dry-run.example.ts.net";
    logStep(`--dry-run (plist generation only), tailnet ${fqdn}`);
  } else {
    logStep("checking Tailscale");
    try {
      const ready = await ensureTailscale({
        resolve: resolveTailscale,
        log: (m) => console.log(`   ${m}`),
        sleep: (ms) => Bun.sleep(ms),
        now: Date.now,
        brewInstall: async () => {
          if (Bun.which("brew")) await runInherit(["brew", "install", "tailscale"]);
        },
      });
      fqdn = ready.fqdn;
    } catch (e) {
      die(e instanceof Error ? e.message : String(e));
    }
    console.log(`   tailnet: ${fqdn}`);
  }
  const coordUrl = `https://${fqdn}:4102`;

  if (binary) {
    // Compiled binary: SPA + migrations embedded, no repo. Skip bun install +
    // vite build; install LaunchAgents that run `roost coord`/`roost worker` via
    // the embedded install scripts (reusing the FRONTED/TLS/serve bash).
    console.log(`   roost: ${process.execPath} (${ROOST_VERSION})`);
    if (!dry) mintCert(fqdn, force);
    await installCoordAgent({
      execPath: process.execPath, gitSha: ROOST_VERSION,
      cmd: dry ? "write-plist" : "install", log: logStep,
    });
    if (dry) {
      await installWorkerAgent({
        execPath: process.execPath, coordUrl, gitSha: ROOST_VERSION,
        cmd: "write-plist", log: logStep,
      });
      console.log(`\n✓ --dry-run complete (LaunchAgent plists generated; nothing installed).`);
      return;
    }
    // Coord must be healthy (DB migrated) before we mint the worker's bootstrap
    // token into it; the worker redeems it on first boot to enroll its key.
    logStep("waiting for coordinator health");
    if (!await waitForCoordHealth(fqdn)) {
      die(`coord did not become healthy at ${coordUrl}`, "check logs: roost logs coord");
    }
    console.log(`   coord healthy at ${coordUrl}`);
    const workerToken = mintWorkerToken(`quickstart-${fqdn}`);
    await installWorkerAgent({
      execPath: process.execPath, coordUrl, bootstrapToken: workerToken,
      gitSha: ROOST_VERSION, cmd: "install", log: logStep,
    });
  } else {
    // From source: deps + build + the on-disk install scripts (unchanged).
    console.log(`   bun: ${process.execPath}`);
    if (force || !existsSync("node_modules")) {
      logStep("bun install");
      if (await runInherit([process.execPath, "install"]) !== 0) die("bun install failed");
    } else {
      logStep("bun install (skipped — node_modules present; --force to reinstall)");
    }
    if (force || !existsSync(WEB_DIST_INDEX)) {
      logStep("building web SPA (apps/web → dist)");
      if (await runInherit([process.execPath, "x", "vite", "build"], "apps/web") !== 0) {
        die("vite build failed");
      }
    } else {
      logStep("web SPA build (skipped — dist present)");
    }
    mintCert(fqdn, force);
    logStep("installing coordinator LaunchAgent");
    if (await runInherit(["bash", "apps/coord/scripts/install.sh", "install"]) !== 0) {
      die("coord install.sh failed");
    }
    logStep("deploying local worker");
    process.env.ROOST_COORDINATOR_URL = coordUrl;
    process.env.ROOST_ALLOW_DIRTY = "1";
    await deploy(["localhost"]);
    logStep("waiting for coordinator health");
    if (!await waitForCoordHealth(fqdn)) {
      die(`coord did not become healthy at ${coordUrl}`, "check logs: roost logs coord");
    }
    console.log(`   coord healthy at ${coordUrl}`);
  }

  // Status readout.
  console.log();
  printStatusReport(await statusReport());

  // Authorize this Mac's browser with no token paste: mint a ?pair token +
  // open it. SPA redeems on load (see sync.ts ?pair= handler).
  logStep("opening the app (browser self-authorizes via ?pair)");
  const token = mintBrowserToken(`quickstart-${fqdn}`);
  const openUrl = `${coordUrl}/?pair=${token}`;
  await runInherit(["open", openUrl]);

  const shim = binary ? null : installRoostShim(process.cwd());
  console.log(`\n✓ Roost is running.`);
  console.log(`  This Mac:        ${openUrl}`);
  console.log(`  Pair your phone: open ${coordUrl} → Settings → Pair a device → scan the QR`);
  if (binary || (shim && shim.onPath)) {
    console.log(`  Health anytime:  roost status`);
  } else if (shim) {
    console.log(`  Health anytime:  ${shim.path} status   (add ~/.bun/bin to PATH for bare \`roost\`)`);
  } else {
    console.log(`  Health anytime:  bun apps/roost-cli/src/main.ts status`);
  }
}

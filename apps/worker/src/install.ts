// First-boot install: redeem bootstrap_token (if present) + register
// with coord. Key generation is owned by jwt.ts::loadWorkerKey — it
// auto-creates an OpenSSH PEM on missing/unparseable file. main.ts
// calls loadWorkerKey BEFORE runInstall, so by the time we get here
// the key exists. Don't duplicate the encoder.
// Callers: main.ts.

import { log } from "@roost/shared";
import type { CoordClient } from "./coord-client.ts";
import type { WorkerConfig as WorkerConfigType } from "@roost/shared";
import { loadWorkerKey } from "./jwt.ts";

// Boot-time coord RPCs MUST time out. runInstall runs BEFORE heartbeat +
// CoordLink start, so a hang here stalls the whole worker boot. A coord
// that's mid-crash leaves the connection half-open (tailscale serve accepts
// the TCP/TLS, the dead coord process never responds) → the await never
// rejects, the try/catch never fires, boot hangs forever (observed: local
// worker stuck 48min while coord segfault-flapped). With a timeout the call
// rejects → caught → boot proceeds → heartbeat + CoordLink retry loops
// self-heal once coord is back.
const BOOT_RPC_TIMEOUT_MS = 10_000;

// vnc:// / smb:// targets are the LIVE tailnet MagicDNS name, not a value
// frozen at install. ROOST_REACHABLE_ADDR went stale the moment a machine was
// renamed in tailscale: the dead name shipped to the SPA and Screen
// Sharing.app couldn't resolve it. Re-resolve every boot so
// renames self-correct. PATH is minimal under a LaunchAgent — probe known
// install dirs. Returns "" when tailscale isn't reachable (env fallback wins).
const TAILSCALE_BINS = [
  "tailscale",
  "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
  "/opt/homebrew/bin/tailscale",
  "/usr/local/bin/tailscale",
];
export function resolveTailnetDnsName(bins: readonly string[] = TAILSCALE_BINS): string {
  for (const bin of bins) {
    try {
      // Bound the spawn: early post-login the Tailscale.app CLI shim talks to
      // a GUI agent that isn't ready yet and `status --json` hangs forever.
      // Unbounded Bun.spawnSync then wedges the worker's main thread and the
      // whole boot (observed: 7.5min stuck on a hung `Tailscale status` child).
      // 2s timeout → SIGKILL → exitCode!=0 → fall through to ROOST_REACHABLE_ADDR.
      const r = Bun.spawnSync([bin, "status", "--json"], { timeout: 2000, killSignal: "SIGKILL" });
      if (r.exitCode !== 0) continue;
      const j = JSON.parse(r.stdout.toString()) as { Self?: { DNSName?: string } };
      const dns = (j.Self?.DNSName ?? "").toLowerCase().replace(/\.$/, "");
      if (dns) return dns;
    } catch { /* try next bin */ }
  }
  return "";
}

/**
 * Redeem bootstrap token (if present) + register with coord.
 * Returns the worker fingerprint.
 */
export async function runInstall(opts: {
  cfg: WorkerConfigType;
  client: CoordClient;
}): Promise<string> {
  const { cfg, client } = opts;
  const key = await loadWorkerKey(cfg.workerKeyPath);
  const fingerprint = key.fingerprint;

  // Redeem bootstrap token (one-shot).
  if (cfg.bootstrapToken) {
    try {
      const os = process.platform === "darwin" ? "darwin" : "linux";
      const git_sha = process.env.GIT_SHA;
      const result = await client.authRedeemWorker({
        token: cfg.bootstrapToken,
        sshPubkeyB64: Buffer.from(key.pubKey).toString("base64"),
        label: cfg.label,
        os,
        ...(git_sha ? { gitSha: git_sha } : {}),
      }, { timeoutMs: BOOT_RPC_TIMEOUT_MS });
      log.info("install", "bootstrap token redeemed", {
        fp: result.fingerprint,
        label: result.label,
      });
      // Clear token from env so it's not accidentally reused.
      delete process.env.ROOST_BOOTSTRAP_TOKEN;
    } catch (e) {
      log.warn("install", "bootstrap redeem failed (may be already used)", { error: String(e) });
    }
  }

  // Register (idempotent for already-known workers).
  try {
    const os = process.platform === "darwin" ? "darwin" : "linux";
    const git_sha = process.env.GIT_SHA;
    const reachable_addr = resolveTailnetDnsName() || process.env.ROOST_REACHABLE_ADDR;
    await client.workersRegister({
      label: cfg.label,
      os,
      ...(git_sha ? { gitSha: git_sha } : {}),
      ...(reachable_addr ? { reachableAddr: reachable_addr } : {}),
    }, { timeoutMs: BOOT_RPC_TIMEOUT_MS });
    log.info("install", "registered with coord", { fingerprint });
  } catch (e) {
    log.warn("install", "register failed (will retry on heartbeat)", { error: String(e) });
  }

  return fingerprint;
}

// First-boot install: redeem bootstrap_token (if present) + register
// with coord. Key generation is owned by jwt.ts::loadWorkerKey — it
// auto-creates an OpenSSH PEM on missing/unparseable file. main.ts
// calls loadWorkerKey BEFORE runInstall, so by the time we get here
// the key exists. Don't duplicate the encoder.
// Callers: main.ts.

import { log, supportedHostPlatform } from "@roost/shared";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import type { CoordClient } from "./coord-client.ts";
import type { WorkerConfig as WorkerConfigType } from "@roost/shared";
import { loadWorkerKey } from "./jwt.ts";
export { resolveTailnetDnsName } from "@roost/shared/tailnet";

// Boot-time coord RPCs MUST time out. runInstall runs BEFORE heartbeat +
// CoordLink start, so a hang here stalls the whole worker boot. A coord
// that's mid-crash leaves the connection half-open (tailscale serve accepts
// the TCP/TLS, the dead coord process never responds) → the await never
// rejects, the try/catch never fires, boot hangs forever (observed: local
// worker stuck 48min while coord segfault-flapped). With a timeout the call
// rejects → caught → boot proceeds → heartbeat + CoordLink retry loops
// self-heal once coord is back.
const BOOT_RPC_TIMEOUT_MS = 10_000;


interface InstallOptions {
  cfg: WorkerConfigType;
  client: CoordClient;
}

async function installWorker(opts: InstallOptions): Promise<string> {
  const { cfg, client } = opts;
  const key = await loadWorkerKey(cfg.workerKeyPath);
  const fingerprint = key.fingerprint;
  const os = supportedHostPlatform();
  // Redeem bootstrap token (one-shot).
  if (cfg.bootstrapToken) {
    try {
      const git_sha = process.env.GIT_SHA;
      const result = await client.authRedeemWorker({
        token: cfg.bootstrapToken,
        sshPubkeyB64: Buffer.from(key.pubKey).toString("base64"),
        label: cfg.label,
        os,
        ...(git_sha ? { gitSha: git_sha } : {}),
      }, { timeoutMs: BOOT_RPC_TIMEOUT_MS });
      if (result.fingerprint !== fingerprint) {
        throw new Error(`bootstrap fingerprint mismatch: expected ${fingerprint}, received ${result.fingerprint}`);
      }
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
    const git_sha = process.env.GIT_SHA;
    const reachable_addr = resolveTailnetDnsName() || process.env.ROOST_REACHABLE_ADDR;
    await client.workersRegister({
      label: cfg.label,
      os,
      ...(git_sha ? { gitSha: git_sha } : {}),
      ...(reachable_addr ? { reachableAddr: reachable_addr } : {}),
    }, { timeoutMs: BOOT_RPC_TIMEOUT_MS });
    log.info("install", "registered with coord", { fingerprint });
  } catch (error) {
    log.warn("install", "register failed (will retry on heartbeat)", { error: String(error) });
  }

  return fingerprint;
}

/** Boot path: best effort so a temporarily unavailable coordinator never
 * prevents the long-lived reconnect loops from starting. */
export function runInstall(opts: InstallOptions): Promise<string> {
  return installWorker(opts);
}

export interface StrictEnrollmentResult {
  fingerprint: string;
  redeemed: boolean;
  registrationConfirmed: boolean;
}

/** Installer path. A lost redemption response is resolved by proving the
 * freshly generated key can perform the authenticated registration. Once that
 * proof succeeds the one-shot token is never needed in the SCM definition. */
export async function runStrictEnrollment(opts: InstallOptions): Promise<StrictEnrollmentResult> {
  const { cfg, client } = opts;
  if (!cfg.bootstrapToken) throw new Error("strict worker enrollment requires a bootstrap token");
  const key = await loadWorkerKey(cfg.workerKeyPath);
  const fingerprint = key.fingerprint;
  const os = supportedHostPlatform();
  const git_sha = process.env.GIT_SHA;
  let redeemed = false;
  let redeemError: unknown;

  try {
    const result = await client.authRedeemWorker({
      token: cfg.bootstrapToken,
      sshPubkeyB64: Buffer.from(key.pubKey).toString("base64"),
      label: cfg.label,
      os,
      ...(git_sha ? { gitSha: git_sha } : {}),
    }, { timeoutMs: BOOT_RPC_TIMEOUT_MS });
    if (result.fingerprint !== fingerprint) {
      throw new Error(`bootstrap fingerprint mismatch: expected ${fingerprint}, received ${result.fingerprint}`);
    }
    redeemed = true;
  } catch (error) {
    redeemError = error;
  }

  let registrationConfirmed = false;
  try {
    const reachable_addr = resolveTailnetDnsName() || process.env.ROOST_REACHABLE_ADDR;
    await client.workersRegister({
      label: cfg.label,
      os,
      ...(git_sha ? { gitSha: git_sha } : {}),
      ...(reachable_addr ? { reachableAddr: reachable_addr } : {}),
    }, { timeoutMs: BOOT_RPC_TIMEOUT_MS });
    registrationConfirmed = true;
  } catch (registrationError) {
    if (!redeemed) {
      throw new Error(
        `bootstrap redemption was not confirmed (${String(redeemError)}); authenticated registration proof also failed (${String(registrationError)})`,
      );
    }
    // Redemption is the irreversible authorization boundary. The token is
    // consumed and this key is now trusted; the installed worker can retry the
    // idempotent registration without ever retaining the token in SCM.
    log.warn("install", "strict enrollment registration pending", {
      fingerprint,
      error: String(registrationError),
    });
  }

  delete process.env.ROOST_BOOTSTRAP_TOKEN;
  log.info("install", "strict enrollment authorized", { fingerprint, redeemed, registrationConfirmed });
  return { fingerprint, redeemed, registrationConfirmed };
}

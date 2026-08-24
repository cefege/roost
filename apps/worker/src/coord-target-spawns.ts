// Child-process runners for the coordinator-relocation transaction, split out
// of coord-target.ts (size ratchet). Every spawn here is wrapped by
// settleWithinTimeout: a wedged tailscale/bun/install.sh used to be able to
// block PREPARE or ABORT indefinitely, holding the relocation's durable state.
// Callers translate RelocationSpawnTimeoutError into the existing rollback path.

import * as fs from "node:fs";
import { join } from "node:path";
import { log } from "@roost/shared/log";

export const RELOCATION_PROBE_TIMEOUT_MS = 10_000;
export const RELOCATION_INSTALLER_TIMEOUT_MS = 120_000;

/** Typed failure for a child killed at its wall clock; the rollback catch path
 *  and logs can distinguish "hung" from "exited non-zero". */
export class RelocationSpawnTimeoutError extends Error {
  constructor(readonly label: string, readonly timeoutMs: number) {
    super(`${label} did not exit within ${timeoutMs}ms`);
    this.name = "RelocationSpawnTimeoutError";
  }
}

/** Hard wall clock around one relocation child. Deliberately NOT Bun.spawn's
 *  built-in `{timeout}`: that only surfaces as a post-hoc signalCode we cannot
 *  distinguish from an external kill or an OOM kill, whereas racing the exit
 *  makes "timed out" provable here. On expiry the child is SIGKILLed so its
 *  pipes close and nothing is left running behind the rollback. */
export async function settleWithinTimeout<T>(
  child: Bun.Subprocess,
  label: string,
  timeoutMs: number,
  consume: () => Promise<T>,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      consume(),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => {
          try { child.kill("SIGKILL"); } catch { /* already exited */ }
          reject(new RelocationSpawnTimeoutError(label, timeoutMs));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export async function captureServeConfig(rollback: string): Promise<void> {
  const config = join(rollback, "tailscale-serve.json");
  const child = Bun.spawn([process.env.ROOST_TAILSCALE_BIN ?? "tailscale", "serve", "get-config", config, "--all"], { stdout: "ignore", stderr: "ignore" });
  // Non-fatal, symmetric with restoreServeConfig: a box that never configured
  // Serve has nothing to restore, and hard-failing PREPARE there would mean a
  // fresh worker could not take the coordinator role at all.
  const code = await settleWithinTimeout(
    child,
    "tailscale serve get-config",
    RELOCATION_PROBE_TIMEOUT_MS,
    () => child.exited,
  );
  if (code !== 0) {
    log.warn("coord-target", "capture_serve_config_failed", { config });
    return;
  }
  if (fs.existsSync(config)) fs.chmodSync(config, 0o600);
}

/** install.sh's serve_front runs `tailscale serve` as the worker's user and
 *  only WARNS when it fails. On Linux that write needs root or an operator
 *  grant, so without one the relocated coordinator binds loopback and is
 *  unreachable at the https URL it advertises — the move gets all the way to
 *  a swapped database before anything notices.
 *
 *  Probing has to attempt a real write: `tailscale serve status` exits 0
 *  without the grant, so reads cannot tell the two states apart. Verified on
 *  a Linux node — denied: "Access denied: serve config denied" (exit 1);
 *  granted: exit 0. A throwaway high port keeps it off anything real, and it
 *  is turned straight back off. */
export async function assertCanFrontCoordinator(platform: string): Promise<void> {
  // Only the fronted mode invokes serve_front (install.sh:299); a direct-TLS
  //  target never calls `tailscale serve`, so its capability is irrelevant.
  if (process.env.ROOST_FRONTED === "0") return;
  switch (platform) {
    case "darwin":
      return; // GUI client runs as the user.
    case "linux":
      break;
    case "win32":
      throw new Error("Windows Tailscale preflight must run inside the relocation transaction");
    default:
      throw new Error(`unsupported coordinator target platform: ${platform}`);
  }
  const ts = process.env.ROOST_TAILSCALE_BIN ?? "tailscale";
  const probe = Bun.spawn([ts, "serve", "--bg", "--https=65535", "http://127.0.0.1:1"], {
    stdout: "pipe", stderr: "pipe",
  });
  const [err, code] = await settleWithinTimeout(
    probe,
    "tailscale serve --bg probe",
    RELOCATION_PROBE_TIMEOUT_MS,
    () => Promise.all([new Response(probe.stderr).text(), probe.exited]),
  );
  if (code === 0) {
    const off = Bun.spawn([ts, "serve", "--https=65535", "off"], { stdout: "ignore", stderr: "ignore" });
    await settleWithinTimeout(off, "tailscale serve off", RELOCATION_PROBE_TIMEOUT_MS, () => off.exited);
    return;
  }
  // --operator=$USER' once"). This string is what the SPA shows an operator,
  // and a blocker that names the fix beats one that only names the fault.
  const detail = err.trim().split("\n").map((l) => l.trim()).filter(Boolean).slice(0, 3).join(" ");
  throw new Error(
    `target cannot configure tailscale serve, so the relocated coordinator would be unreachable at its public URL: ${detail || `exit ${code}`}`,
  );
}

export async function restoreServeConfig(rollback: string): Promise<void> {
  const config = join(rollback, "tailscale-serve.json");
  // `serve get-config` on a machine with no serve config yields an empty
  // document that `set-config` rejects; the throw used to escape abort() and
  // surface as "target rollback failed".
  let captured: string;
  try { captured = fs.readFileSync(config, "utf8").trim(); } catch { return; }
  if (!captured || captured === "{}" || captured === "null") return;

  const child = Bun.spawn([process.env.ROOST_TAILSCALE_BIN ?? "tailscale", "serve", "set-config", config, "--all"], { stdout: "ignore", stderr: "ignore" });
  const code = await settleWithinTimeout(
    child,
    "tailscale serve set-config",
    RELOCATION_PROBE_TIMEOUT_MS,
    () => child.exited,
  );
  if (code !== 0) {
    log.warn("coord-target", "restore_serve_config_failed", { config });
  }
}

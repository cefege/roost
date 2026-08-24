// Windows identity helpers shared by service-ctl, the binary installer, the
// relocation broker, and the CLI's tailscale front-doors: service-account
// normalization for SCM comparisons, and the two trust contexts under which
// a tailscale executable may be resolved. One copy exists because account
// spellings and executable paths cross module boundaries during rollback
// proofs — a per-module normalization drift reads as a phantom config change.
//
// Account normalization deliberately strips BOTH local-machine prefixes
// (".\alice" and "./alice") before case-folding: SCM treats a bare name and
// its ".\"-qualified form as the same account, so equality proofs must too,
// and the operator-account denylist stays hole-free for either spelling.

import { lstatSync } from "node:fs";
import { win32 } from "node:path";

/**
 * Canonical form used for every Windows service-account comparison: trim,
 * drop the machine prefix, case-fold en-US.
 */
export function normalizedWindowsAccount(account: string): string {
  return account.trim().replace(/^[.][\\/]/, "").toLocaleLowerCase("en-US");
}

export interface TrustedTailscaleResolution {
  /** Service context resolves only the admin-owned ProgramFiles install;
   *  interactive CLI context accepts a validated ROOST_TAILSCALE_EXE
   *  override and falls back to PATH off Windows. */
  serviceContext?: boolean;
}

/** Resolve the tailscale executable under one of the two standing trust
 *  models. The service-context branch ignores the environment entirely: an
 *  SCM-invoked broker must not let an inherited variable redirect execution. */
export function trustedTailscaleExecutable(options: TrustedTailscaleResolution = {}): string {
  if (options.serviceContext) {
    const programFiles = process.env.ProgramFiles;
    if (!programFiles || !win32.isAbsolute(programFiles)) {
      throw new Error("trusted ProgramFiles path is unavailable");
    }
    return win32.join(programFiles, "Tailscale", "tailscale.exe");
  }
  if (process.platform !== "win32") return "tailscale";
  const executable = process.env.ROOST_TAILSCALE_EXE?.trim();
  if (!executable || !win32.isAbsolute(executable) || /[\0\r\n]/.test(executable)) {
    throw new Error("Windows quickstart requires the trusted absolute ROOST_TAILSCALE_EXE");
  }
  const info = lstatSync(executable);
  if (!info.isFile() || info.isSymbolicLink()) {
    throw new Error("ROOST_TAILSCALE_EXE must be a non-reparse regular file");
  }
  return executable;
}

/** Run one trusted (service-context) tailscale command, capturing output and
 *  bounding the wait — `serve` reconfiguration is on the coordinator cutover
 *  path and must not hang the relocation broker past its journal deadlines. */
export async function runTrustedTailscale(args: string[]): Promise<void> {
  const child = Bun.spawn([trustedTailscaleExecutable({ serviceContext: true }), ...args], {
    stdout: "pipe",
    stderr: "pipe",
    signal: AbortSignal.timeout(30_000),
  });
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  if (code !== 0) {
    const detail = `${stderr}\n${stdout}`.trim().replace(/[\r\n]+/g, " ").slice(0, 1024);
    throw new Error(`trusted Tailscale command failed (${code})${detail ? `: ${detail}` : ""}`);
  }
}

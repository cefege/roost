// crpc6 — extracted from router/workers.ts so the Connect handler can
// call into it without going through the tRPC procedure shim. Owns the
// in-memory DeployJob registry + spawn helpers.
//
// The signed Windows-update jobs that share this registry live in
// windows-update-deploy-jobs.ts (+ -record/-runtime); this file is the generic
// registry and the POSIX `roost deploy` subprocess.

import { BoundedBus } from "./buses.ts";
import { busToAsyncIterable } from "./sse.ts";
import { signal } from "@roost/shared/diag";
import { log } from "@roost/shared/log";
import type { SignalKind } from "@roost/shared/diag";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import { IS_COMPILED_ROOST_BUILD } from "@roost/shared/build-identity";
import { durableRemove } from "@roost/shared/durability";
import {
  WINDOWS_UPDATE_POLL_MS,
  isDeployJobId,
  windowsUpdateDeployRecordPath,
} from "./windows-update-deploy-record.ts";
import { recoverWindowsUpdateJob } from "./windows-update-deploy-jobs.ts";

export type DeployStreamMsg =
  | { kind: "line"; text: string }
  | { kind: "done"; exit: number | null; error?: string };

export interface DeployJob {
  jobId: string;
  host: string;
  startedAt: number;
  updatedAt?: number;
  completedAt?: number;
  lines: string[];
  status: "running" | "done";
  exitCode?: number | null;
  error?: string;
  bus: BoundedBus<DeployStreamMsg>;
  gcTimer: Timer | null;
  windowsUpdate?: {
    workerFp: string;
    manifestUrl: string;
    signatureUrl: string;
    manifestSha256: string;
    publisherSha256: string;
    lastSequence: number;
    inFlight: boolean;
    startAccepted: boolean;
    pollTimer: Timer | null;
    deadlineTimer: Timer | null;
    lastTransportError?: string;
    durable: boolean;
    mutationTail: Promise<void>;
  };
}

export const _deployJobs = new Map<string, DeployJob>();

// Child-originated signal kinds the ROOST_SIGNAL sentinel bridge will forward.
// Runtime-membership check on dynamically parsed subprocess output — the
// `kind as SignalKind` cast in emitLine bypasses compile-time typo-safety,
// so this allowlist is what keeps a phantom/unknown kind out of doctor.
// (`deploy.failed` fires from emitDone on exit code, not via the bridge.)
const KNOWN_DEPLOY_SIGNALS = new Set(["deploy.cert_skipped"]);
export const DEPLOY_JOB_TTL_MS = 20 * 60 * 1000;
export function _gcJob(jobId: string, completedAt = Date.now()): void {
  const job = _deployJobs.get(jobId);
  if (!job) return;
  clearTimeout(job.gcTimer ?? undefined);
  const expire = async (): Promise<void> => {
    const current = _deployJobs.get(jobId);
    if (current !== job) return;
    if (current.windowsUpdate?.durable) {
      try {
        await durableRemove(windowsUpdateDeployRecordPath(jobId), {
          mode: 0o600,
          privateDacl: true,
        });
      } catch (error) {
        // The record survives until this succeeds; say which one so a stuck
        // GC loop is diagnosable instead of silently rescheduling forever.
        log.warn("windows-update", "gc_record_remove_failed", {
          job_id: jobId, record: windowsUpdateDeployRecordPath(jobId),
          error: String(error),
        });
        current.gcTimer = setTimeout(() => void expire(), WINDOWS_UPDATE_POLL_MS);
        return;
      }
    }
    clearTimeout(current.windowsUpdate?.pollTimer ?? undefined);
    clearTimeout(current.windowsUpdate?.deadlineTimer ?? undefined);
    _deployJobs.delete(jobId);
  };
  job.gcTimer = setTimeout(
    () => void expire(),
    Math.max(0, completedAt + DEPLOY_JOB_TTL_MS - Date.now()),
  );
}

export interface DeployStartResult {
  ok: boolean;
  jobId?: string;
  error?: string;
}

export function resolveDeployCoordinatorUrl(
  env: Record<string, string | undefined>,
  tailnetDnsName: string,
): string | null {
  const configured = env.ROOST_COORDINATOR_URL ?? env.ROOST_COORDINATOR_PUBLIC_URL;
  if (configured) return configured;
  const host = env.ROOST_REACHABLE_ADDR || tailnetDnsName;
  if (!host.endsWith(".ts.net")) return null;
  // ROOST_COORDINATOR_BIND is the private loopback listener (normally :4103)
  // when Tailscale Serve fronts the coordinator. Workers need the advertised
  // tailnet port, not that internal listener.
  const port = env.ROOST_TAILNET_HTTPS_PORT || "4102";
  return `https://${host}:${port}`;
}

export function startDeploy(host: string): DeployStartResult {
  if (!/^[A-Za-z0-9.-]+$/.test(host)) {
    return { ok: false, error: "invalid host" };
  }
  if (IS_COMPILED_ROOST_BUILD) {
    return {
      ok: false,
      error: "POSIX source deployment requires a coordinator source checkout; run `roost push` from that checkout",
    };
  }
  const repoRoot = process.cwd();
  const coordUrl = resolveDeployCoordinatorUrl(
    process.env,
    resolveTailnetDnsName(),
  );
  if (!coordUrl || coordUrl.includes("//localhost") || coordUrl.includes("//127.0.0.1") || coordUrl.includes(".local:")) {
    return { ok: false, error: `coord has no tailnet-reachable URL (resolved="${coordUrl ?? "none"}"). Set ROOST_REACHABLE_ADDR=<tailnet-fqdn>.` };
  }
  const env = { ...process.env, ROOST_COORDINATOR_URL: coordUrl };
  const DEPLOY_TIMEOUT_MS = 180_000;
  const bunBin = process.execPath;
  const jobId = crypto.randomUUID();
  const job: DeployJob = {
    jobId, host, startedAt: Date.now(),
    lines: [], status: "running",
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
  };
  _deployJobs.set(jobId, job);

  function emitLine(text: string): void {
    const trimmed = text.replace(/\r$/, "");
    // Subprocess→coord signal bridge: the detached `roost deploy` child's
    // stderr only reaches this ephemeral bus, so it emits `ROOST_SIGNAL
    // <kind> [json-kv]` sentinels that we lift into durable signals here.
    if (trimmed.startsWith("ROOST_SIGNAL ")) {
      const rest = trimmed.slice("ROOST_SIGNAL ".length).trimStart();
      const sp = rest.indexOf(" ");
      const kind = sp === -1 ? rest : rest.slice(0, sp);
      if (KNOWN_DEPLOY_SIGNALS.has(kind)) {
        let kv: Record<string, unknown> = {};
        if (sp !== -1) {
          try {
            const parsed = JSON.parse(rest.slice(sp + 1));
            if (parsed && typeof parsed === "object") kv = parsed as Record<string, unknown>;
          } catch { /* malformed kv → forward the sentinel's kind with no detail */ }
        }
        signal(kind as SignalKind, { host, ...kv, cooldownKey: host });
        return;
      }
      // Unknown/typo'd sentinel — never emit a phantom signal; fall through
      // and publish it as an ordinary deploy line.
    }
    job.lines.push(trimmed);
    job.bus.publish({ kind: "line", text: trimmed });
  }
  function emitDone(exit: number | null, error?: string): void {
    job.status = "done";
    job.exitCode = exit;
    if (error) job.error = error;
    if (error) signal("deploy.failed", { host, exit, reason: error, cooldownKey: host });
    job.bus.publish({ kind: "done", exit, error });
    _gcJob(jobId);
  }

  try {
    const proc = Bun.spawn({
      cmd: IS_COMPILED_ROOST_BUILD
        ? [bunBin, "deploy", host]
        : [bunBin, "apps/roost-cli/src/main.ts", "deploy", host],
      cwd: repoRoot, env,
      stdout: "pipe", stderr: "pipe",
    });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      try { proc.kill(); } catch { /* ignore */ }
    }, DEPLOY_TIMEOUT_MS);

    async function pump(stream: ReadableStream<Uint8Array>): Promise<void> {
      const reader = stream.getReader();
      const dec = new TextDecoder();
      let buf = "";
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        let idx: number;
        while ((idx = buf.indexOf("\n")) !== -1) {
          emitLine(buf.slice(0, idx));
          buf = buf.slice(idx + 1);
        }
      }
      // Flush any trailing partial-multibyte sequence held in the
      // decoder's internal buffer. Without this final no-stream
      // decode, a non-ASCII path that lands at the very end of the
      // rsync output silently loses its last 1-3 bytes.
      buf += dec.decode();
      if (buf.length > 0) emitLine(buf);
    }

    void Promise.all([pump(proc.stdout), pump(proc.stderr), proc.exited])
      .then(([, , exit]) => {
        clearTimeout(timer);
        if (timedOut) emitDone(exit ?? null, `deploy timed out after ${DEPLOY_TIMEOUT_MS / 1000}s`);
        else if (exit !== 0) emitDone(exit ?? null, `deploy exit ${exit}`);
        else emitDone(exit ?? 0);
      })
      .catch((e) => {
        clearTimeout(timer);
        emitDone(null, (e as Error).message);
      });
    return { ok: true, jobId };
  } catch (e) {
    emitDone(null, (e as Error).message);
    return { ok: true, jobId };
  }
}

export function __clearDeployJobsForTest(): void {
  for (const job of _deployJobs.values()) {
    clearTimeout(job.gcTimer ?? undefined);
    clearTimeout(job.windowsUpdate?.pollTimer ?? undefined);
    clearTimeout(job.windowsUpdate?.deadlineTimer ?? undefined);
    job.status = "done";
  }
  _deployJobs.clear();
}

export async function* deployOutput(jobId: string, signal?: AbortSignal): AsyncGenerator<DeployStreamMsg> {
  if (!isDeployJobId(jobId)) {
    yield { kind: "done", exit: null, error: "unknown jobId" };
    return;
  }
  let job = _deployJobs.get(jobId);
  if (!job) job = await recoverWindowsUpdateJob(jobId);
  if (!job) {
    yield { kind: "done", exit: null, error: "unknown jobId" };
    return;
  }
  for (const text of job.lines) yield { kind: "line", text };
  if (job.status === "done") {
    yield { kind: "done", exit: job.exitCode ?? null, error: job.error };
    return;
  }
  for await (const message of busToAsyncIterable(job.bus, {
    signal,
    // Byte-weight the queue so a flood of long lines trips the byte cap
    // before the frame cap; text.length approximates UTF-8 cost closely
    // enough for a bound.
    sizeOf: (message) => (message.kind === "line" ? message.text.length : 32),
  })) {
    yield message;
    if (message.kind === "done") return;
  }
}

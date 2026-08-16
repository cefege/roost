// crpc6 — extracted from router/workers.ts so the Connect handler can
// call into it without going through the tRPC procedure shim. Owns the
// in-memory DeployJob registry + spawn helpers.

import { BoundedBus } from "./buses.ts";
import { busToAsyncIterable } from "./sse.ts";
import { signal } from "@roost/shared/diag";
import type { SignalKind } from "@roost/shared/diag";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import { sendWindowsUpdateBroker } from "./connect/worker-send.ts";

export type DeployStreamMsg =
  | { kind: "line"; text: string }
  | { kind: "done"; exit: number | null; error?: string };

interface DeployJob {
  jobId: string;
  host: string;
  startedAt: number;
  lines: string[];
  status: "running" | "done";
  exitCode?: number | null;
  error?: string;
  bus: BoundedBus<DeployStreamMsg>;
  windowsUpdate?: {
    workerFp: string;
    manifestUrl: string;
    signatureUrl: string;
    manifestSha256: string;
    publisherSha256: string;
    lastSequence: number;
    inFlight: boolean;
    pollTimer: Timer | null;
    deadlineTimer: Timer | null;
    lastTransportError?: string;
  };
}

const _deployJobs = new Map<string, DeployJob>();

// Child-originated signal kinds the ROOST_SIGNAL sentinel bridge will forward.
// Runtime-membership check on dynamically parsed subprocess output — the
// `kind as SignalKind` cast in emitLine bypasses compile-time typo-safety,
// so this allowlist is what keeps a phantom/unknown kind out of doctor.
// (`deploy.failed` fires from emitDone on exit code, not via the bridge.)
const KNOWN_DEPLOY_SIGNALS = new Set(["deploy.cert_skipped"]);
const DEPLOY_JOB_TTL_MS = 5 * 60 * 1000;
function _gcJob(jobId: string): void {
  setTimeout(() => {
    const job = _deployJobs.get(jobId);
    clearTimeout(job?.windowsUpdate?.pollTimer ?? undefined);
    clearTimeout(job?.windowsUpdate?.deadlineTimer ?? undefined);
    _deployJobs.delete(jobId);
  }, DEPLOY_JOB_TTL_MS);
}

export interface DeployStartResult {
  ok: boolean;
  jobId?: string;
  error?: string;
}

export function startDeploy(host: string): DeployStartResult {
  if (!/^[A-Za-z0-9.-]+$/.test(host)) {
    return { ok: false, error: "invalid host" };
  }
  const repoRoot = process.cwd();
  const port = (process.env.ROOST_COORDINATOR_BIND ?? "0.0.0.0:4102").split(":").pop() ?? "4102";
  let coordUrl: string | null = process.env.ROOST_COORDINATOR_URL ?? null;
  if (!coordUrl && process.env.ROOST_REACHABLE_ADDR) {
    coordUrl = `https://${process.env.ROOST_REACHABLE_ADDR}:${port}`;
  }
  if (!coordUrl) {
    const dns = resolveTailnetDnsName();
    if (dns.endsWith(".ts.net")) coordUrl = `https://${dns}:${port}`;
  }
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
      cmd: [bunBin, "apps/roost-cli/src/main.ts", "deploy", host],
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

export interface WindowsUpdateDeployOptions {
  workerFp: string;
  manifestUrl: string;
  signatureUrl: string;
  manifestSha256: string;
  publisherSha256: string;
}

const WINDOWS_UPDATE_TIMEOUT_MS = 15 * 60 * 1000;
const WINDOWS_UPDATE_POLL_MS = 2_000;
const MAX_PERSISTED_UPDATE_LINES = 2_048;

function emitWindowsUpdateLine(job: DeployJob, text: string): void {
  const line = text.replace(/\r?\n/g, " ").trim();
  if (!line) return;
  if (job.lines.length >= MAX_PERSISTED_UPDATE_LINES) job.lines.shift();
  job.lines.push(line);
  job.bus.publish({ kind: "line", text: line });
}

function finishWindowsUpdate(job: DeployJob, exit: number, error?: string): void {
  if (job.status === "done") return;
  job.status = "done";
  job.exitCode = exit;
  job.error = error;
  const update = job.windowsUpdate;
  if (update?.pollTimer) {
    clearTimeout(update.pollTimer);
    update.pollTimer = null;
  }
  if (update?.deadlineTimer) {
    clearTimeout(update.deadlineTimer);
    update.deadlineTimer = null;
  }
  if (error) {
    signal("deploy.failed", { host: job.host, exit, reason: error, cooldownKey: job.host });
  }
  job.bus.publish({ kind: "done", exit, error });
  _gcJob(job.jobId);
}

function scheduleWindowsUpdateStatus(job: DeployJob, delayMs = WINDOWS_UPDATE_POLL_MS): void {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running" || update.pollTimer) return;
  update.pollTimer = setTimeout(() => {
    update.pollTimer = null;
    void issueWindowsUpdateCommand(job, "STATUS");
  }, delayMs);
}

async function issueWindowsUpdateCommand(job: DeployJob, action: "START" | "STATUS"): Promise<void> {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running" || update.inFlight) return;
  update.inFlight = true;
  try {
    const pending = sendWindowsUpdateBroker(update.workerFp, {
      jobId: job.jobId,
      action,
      manifestUrl: action === "START" ? update.manifestUrl : undefined,
      signatureUrl: action === "START" ? update.signatureUrl : undefined,
      manifestSha256: action === "START" ? update.manifestSha256 : undefined,
      publisherSha256: action === "START" ? update.publisherSha256 : undefined,
    });
    await pending.promise;
    update.lastTransportError = undefined;
  } catch (error) {
    // The expected update path stops/restarts worker while RoostUpdaterV2
    // remains outside its Shawl job. Keep the deploy job alive and ask the
    // reconnected worker to replay its journal instead of treating the lost
    // command ACK as update failure.
    const message = (error as Error).message;
    if (update.lastTransportError !== message) {
      update.lastTransportError = message;
      emitWindowsUpdateLine(job, `worker link unavailable; waiting for persisted updater progress (${message})`);
    }
  } finally {
    update.inFlight = false;
    scheduleWindowsUpdateStatus(job);
  }
}

export function startWindowsUpdateDeploy(host: string, options: WindowsUpdateDeployOptions): DeployStartResult {
  if (!/^[A-Za-z0-9.-]+$/.test(host)) return { ok: false, error: "invalid host" };
  if (!/^[a-f0-9]{64}$/.test(options.workerFp)) return { ok: false, error: "invalid worker fingerprint" };
  if (!/^[a-f0-9]{64}$/i.test(options.manifestSha256)) return { ok: false, error: "invalid manifest digest" };
  if (options.publisherSha256 && !/^[a-f0-9]{64}$/i.test(options.publisherSha256)) {
    return { ok: false, error: "invalid Windows publisher pin" };
  }
  try {
    if (new URL(options.manifestUrl).protocol !== "https:" || new URL(options.signatureUrl).protocol !== "https:") {
      return { ok: false, error: "Windows update manifest URLs must use HTTPS" };
    }
  } catch {
    return { ok: false, error: "invalid Windows update manifest URL" };
  }

  const jobId = crypto.randomUUID();
  const job: DeployJob = {
    jobId,
    host,
    startedAt: Date.now(),
    lines: [],
    status: "running",
    bus: new BoundedBus<DeployStreamMsg>(2048),
    windowsUpdate: {
      workerFp: options.workerFp,
      manifestUrl: options.manifestUrl,
      signatureUrl: options.signatureUrl,
      manifestSha256: options.manifestSha256.toLowerCase(),
      publisherSha256: options.publisherSha256.toLowerCase(),
      lastSequence: -1,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
    },
  };
  _deployJobs.set(jobId, job);
  job.windowsUpdate!.deadlineTimer = setTimeout(() => {
    finishWindowsUpdate(job, 1, `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`);
  }, WINDOWS_UPDATE_TIMEOUT_MS);
  emitWindowsUpdateLine(job, `starting signed Windows update on ${host}`);
  void issueWindowsUpdateCommand(job, "START");
  return { ok: true, jobId };
}

export function handleWorkerUpdateProgress(workerFp: string, progress: {
  request_id: string;
  job_id: string;
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}): void {
  const job = _deployJobs.get(progress.job_id);
  const update = job?.windowsUpdate;
  if (!job || !update || update.workerFp !== workerFp || job.status !== "running") return;
  if (progress.sequence <= update.lastSequence) return;
  update.lastSequence = progress.sequence;
  update.lastTransportError = undefined;
  const detail = progress.message || progress.error || progress.phase;
  emitWindowsUpdateLine(job, `[${progress.phase || "update"}] ${detail}`);
  if (progress.terminal) {
    finishWindowsUpdate(job, progress.success ? 0 : 1, progress.success ? undefined : (progress.error || "Windows update failed"));
  } else {
    scheduleWindowsUpdateStatus(job);
  }
}

export function resumeWindowsUpdateDeploysForWorker(workerFp: string): void {
  for (const job of _deployJobs.values()) {
    const update = job.windowsUpdate;
    if (!update || update.workerFp !== workerFp || job.status !== "running") continue;
    if (update.pollTimer) {
      clearTimeout(update.pollTimer);
      update.pollTimer = null;
    }
    void issueWindowsUpdateCommand(job, "STATUS");
  }
}

export async function* deployOutput(jobId: string, signal?: AbortSignal): AsyncGenerator<DeployStreamMsg> {
  const job = _deployJobs.get(jobId);
  if (!job) {
    yield { kind: "done", exit: null, error: "unknown jobId" };
    return;
  }
  for (const text of job.lines) yield { kind: "line", text };
  if (job.status === "done") {
    yield { kind: "done", exit: job.exitCode ?? null, error: job.error };
    return;
  }
  for await (const message of busToAsyncIterable(job.bus, { signal })) {
    yield message;
    if (message.kind === "done") return;
  }
}

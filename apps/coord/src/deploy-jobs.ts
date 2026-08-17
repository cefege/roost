// crpc6 — extracted from router/workers.ts so the Connect handler can
// call into it without going through the tRPC procedure shim. Owns the
// in-memory DeployJob registry + spawn helpers.

import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";

import { BoundedBus } from "./buses.ts";
import { busToAsyncIterable } from "./sse.ts";
import { signal } from "@roost/shared/diag";
import type { SignalKind } from "@roost/shared/diag";
import { resolveTailnetDnsName } from "@roost/shared/tailnet";
import { sendWindowsUpdateBroker } from "./connect/worker-send.ts";
import { IS_COMPILED_ROOST_BUILD } from "@roost/shared/build-identity";
import { coordDataDir } from "@roost/shared/paths";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import { Code, ConnectError } from "@connectrpc/connect";

export type DeployStreamMsg =
  | { kind: "line"; text: string }
  | { kind: "done"; exit: number | null; error?: string };

interface DeployJob {
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

const _deployJobs = new Map<string, DeployJob>();

// Child-originated signal kinds the ROOST_SIGNAL sentinel bridge will forward.
// Runtime-membership check on dynamically parsed subprocess output — the
// `kind as SignalKind` cast in emitLine bypasses compile-time typo-safety,
// so this allowlist is what keeps a phantom/unknown kind out of doctor.
// (`deploy.failed` fires from emitDone on exit code, not via the bridge.)
const KNOWN_DEPLOY_SIGNALS = new Set(["deploy.cert_skipped"]);
const DEPLOY_JOB_TTL_MS = 20 * 60 * 1000;
function _gcJob(jobId: string, completedAt = Date.now()): void {
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
      } catch {
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
const MAX_PERSISTED_UPDATE_LINE_LENGTH = 2_048;
const MAX_PERSISTED_UPDATE_RECORD_BYTES = 8 * 1024 * 1024;
const MAX_PERSISTED_TIMESTAMP_SKEW_MS = 5 * 60 * 1000;
const DEPLOY_JOB_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const WINDOWS_WORKER_FP_RE = /^[0-9a-f]{64}$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const HOST_RE = /^[A-Za-z0-9.-]+$/;

const httpsUrlSchema = z.string().min(1).max(2_048).url().refine((value) => {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password;
  } catch {
    return false;
  }
}, "URL must use HTTPS without credentials");

const persistedWindowsUpdateCommon = {
  schemaVersion: z.literal(1),
  jobId: z.string().regex(DEPLOY_JOB_ID_RE),
  host: z.string().min(1).max(253).regex(HOST_RE),
  workerFp: z.string().regex(WINDOWS_WORKER_FP_RE),
  manifestUrl: httpsUrlSchema,
  signatureUrl: httpsUrlSchema,
  manifestSha256: z.string().regex(SHA256_RE),
  publisherSha256: z.union([z.literal(""), z.string().regex(SHA256_RE)]),
  startedAt: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
  updatedAt: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
  startAccepted: z.boolean(),
  lastSequence: z.number().int().min(-1).max(Number.MAX_SAFE_INTEGER),
  lines: z.array(z.string().min(1).max(MAX_PERSISTED_UPDATE_LINE_LENGTH))
    .max(MAX_PERSISTED_UPDATE_LINES),
};

const persistedWindowsUpdateRunningSchema = z.object({
  ...persistedWindowsUpdateCommon,
  status: z.literal("running"),
  completedAt: z.null(),
  exitCode: z.null(),
  error: z.null(),
}).strict();

const persistedWindowsUpdateDoneSchema = z.object({
  ...persistedWindowsUpdateCommon,
  status: z.literal("done"),
  completedAt: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
  exitCode: z.union([z.literal(0), z.literal(1)]),
  error: z.string().min(1).max(MAX_PERSISTED_UPDATE_LINE_LENGTH).nullable(),
}).strict();

const persistedWindowsUpdateSchema = z.discriminatedUnion("status", [
  persistedWindowsUpdateRunningSchema,
  persistedWindowsUpdateDoneSchema,
]).superRefine((record, context) => {
  if (record.updatedAt < record.startedAt) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "updatedAt precedes startedAt" });
  }
  if (record.manifestUrl === record.signatureUrl) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "manifest URLs must be distinct" });
  }
  if (record.lastSequence >= 0 && !record.startAccepted) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "progress requires accepted START" });
  }
  if (record.status === "done"
    && (record.completedAt < record.startedAt || record.updatedAt < record.completedAt)) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "invalid terminal timestamps" });
  }
  if (record.status === "done"
    && ((record.exitCode === 0 && record.error !== null)
      || (record.exitCode === 1 && record.error === null))) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "terminal result is inconsistent" });
  }
});

type PersistedWindowsUpdateJob = z.infer<typeof persistedWindowsUpdateSchema>;
type PersistedWindowsUpdateLoad =
  | { kind: "record"; record: PersistedWindowsUpdateJob }
  | { kind: "missing" | "invalid" | "expired" };

function isDeployJobId(jobId: string): boolean {
  return DEPLOY_JOB_ID_RE.test(jobId);
}

export function windowsUpdateDeployRecordPath(jobId: string): string {
  if (!isDeployJobId(jobId)) throw new Error("invalid Windows update jobId");
  return join(coordDataDir(), "windows-update-jobs", `${jobId}.json`);
}

function normalizeWindowsUpdateLine(text: string): string | null {
  const line = text.replace(/\r?\n/g, " ").trim().slice(0, MAX_PERSISTED_UPDATE_LINE_LENGTH);
  return line || null;
}

function linesWithWindowsUpdateLine(lines: readonly string[], line: string): string[] {
  const start = Math.max(0, lines.length - MAX_PERSISTED_UPDATE_LINES + 1);
  return [...lines.slice(start), line];
}

function emitWindowsUpdateLine(job: DeployJob, text: string): void {
  const line = normalizeWindowsUpdateLine(text);
  if (!line) return;
  job.lines = linesWithWindowsUpdateLine(job.lines, line);
  job.bus.publish({ kind: "line", text: line });
}

function persistedWindowsUpdateRecord(
  job: DeployJob,
  overrides: Record<string, unknown> = {},
): PersistedWindowsUpdateJob {
  const update = job.windowsUpdate;
  if (!update?.durable) throw new Error("Windows update job has no durable coordinator record");
  return persistedWindowsUpdateSchema.parse({
    schemaVersion: 1,
    jobId: job.jobId,
    host: job.host,
    workerFp: update.workerFp,
    manifestUrl: update.manifestUrl,
    signatureUrl: update.signatureUrl,
    manifestSha256: update.manifestSha256,
    publisherSha256: update.publisherSha256,
    status: job.status,
    startedAt: job.startedAt,
    updatedAt: job.updatedAt ?? job.startedAt,
    completedAt: job.completedAt ?? null,
    startAccepted: update.startAccepted,
    lastSequence: update.lastSequence,
    lines: job.lines,
    exitCode: job.exitCode ?? null,
    error: job.error ?? null,
    ...overrides,
  });
}

async function persistWindowsUpdateRecord(record: PersistedWindowsUpdateJob): Promise<void> {
  await durableWriteFile(
    windowsUpdateDeployRecordPath(record.jobId),
    `${JSON.stringify(record)}\n`,
    { mode: 0o600, privateDacl: true },
  );
}


async function loadPersistedWindowsUpdate(jobId: string): Promise<PersistedWindowsUpdateLoad> {
  let raw: string;
  try {
    raw = await readFile(windowsUpdateDeployRecordPath(jobId), "utf8");
  } catch (error) {
    const code = error instanceof Error && "code" in error
      ? (error as NodeJS.ErrnoException).code
      : undefined;
    return { kind: code === "ENOENT" ? "missing" : "invalid" };
  }
  if (Buffer.byteLength(raw) > MAX_PERSISTED_UPDATE_RECORD_BYTES) return { kind: "invalid" };
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return { kind: "invalid" };
  }
  const parsed = persistedWindowsUpdateSchema.safeParse(value);
  if (!parsed.success || parsed.data.jobId !== jobId) return { kind: "invalid" };
  const record = parsed.data;
  const latestTimestamp = record.status === "done"
    ? Math.max(record.startedAt, record.updatedAt, record.completedAt)
    : Math.max(record.startedAt, record.updatedAt);
  if (latestTimestamp > Date.now() + MAX_PERSISTED_TIMESTAMP_SKEW_MS) return { kind: "invalid" };
  const expiresAt = record.status === "done"
    ? record.completedAt + DEPLOY_JOB_TTL_MS
    : record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS + DEPLOY_JOB_TTL_MS;
  if (expiresAt <= Date.now()) {
    await durableRemove(windowsUpdateDeployRecordPath(jobId), {
      mode: 0o600,
      privateDacl: true,
    });
    return { kind: "expired" };
  }
  return { kind: "record", record };
}

function enqueueWindowsUpdateMutation(job: DeployJob, operation: () => Promise<void>): Promise<void> {
  const update = job.windowsUpdate;
  if (!update) return Promise.resolve();
  const next = update.mutationTail.catch(() => undefined).then(operation);
  update.mutationTail = next;
  return next;
}

function armWindowsUpdateDeadline(job: DeployJob, delayMs: number, error: string): void {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running") return;
  clearTimeout(update.deadlineTimer ?? undefined);
  update.deadlineTimer = setTimeout(() => {
    update.deadlineTimer = null;
    void finishWindowsUpdate(job, 1, error);
  }, Math.max(0, delayMs));
}

async function finishWindowsUpdateNow(
  job: DeployJob,
  exit: 0 | 1,
  error?: string,
  finalLine?: string,
  acceptedProgress?: { sequence: number },
  completedAt = Date.now(),
): Promise<void> {
  if (job.status === "done") return;
  const update = job.windowsUpdate;
  if (!update) return;
  const line = finalLine ? normalizeWindowsUpdateLine(finalLine) : null;
  const lines = line ? linesWithWindowsUpdateLine(job.lines, line) : job.lines;
  const normalizedError = error
    ? normalizeWindowsUpdateLine(error) ?? "Windows update failed"
    : undefined;
  if (update.durable) {
    await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job, {
      status: "done",
      updatedAt: completedAt,
      completedAt,
      lines,
      exitCode: exit,
      error: normalizedError ?? null,
      ...(acceptedProgress
        ? { startAccepted: true, lastSequence: acceptedProgress.sequence }
        : {}),
    }));
  }
  if (acceptedProgress) {
    update.startAccepted = true;
    update.lastSequence = acceptedProgress.sequence;
    update.lastTransportError = undefined;
  }
  job.lines = lines;
  job.status = "done";
  job.updatedAt = completedAt;
  job.completedAt = completedAt;
  job.exitCode = exit;
  job.error = normalizedError;
  if (update.pollTimer) {
    clearTimeout(update.pollTimer);
    update.pollTimer = null;
  }
  if (update.deadlineTimer) {
    clearTimeout(update.deadlineTimer);
    update.deadlineTimer = null;
  }
  if (line) job.bus.publish({ kind: "line", text: line });
  if (normalizedError) {
    signal("deploy.failed", {
      host: job.host,
      exit,
      reason: normalizedError,
      cooldownKey: job.host,
    });
  }
  job.bus.publish({ kind: "done", exit, error: normalizedError });
  _gcJob(job.jobId, completedAt);
}

async function finishWindowsUpdate(
  job: DeployJob,
  exit: 0 | 1,
  error?: string,
  completedAt?: number,
): Promise<void> {
  try {
    await enqueueWindowsUpdateMutation(
      job,
      () => finishWindowsUpdateNow(job, exit, error, undefined, undefined, completedAt),
    );
  } catch (persistError) {
    if (job.status !== "running") return;
    const message = persistError instanceof Error ? persistError.message : String(persistError);
    emitWindowsUpdateLine(job, `coordinator update journal unavailable; retrying terminal commit (${message})`);
    const update = job.windowsUpdate;
    if (update && !update.deadlineTimer) {
      update.deadlineTimer = setTimeout(() => {
        update.deadlineTimer = null;
        void finishWindowsUpdate(job, exit, error, completedAt);
      }, WINDOWS_UPDATE_POLL_MS);
    }
  }
}

function createTransientRecoveredWindowsUpdateJob(jobId: string, workerFp = ""): DeployJob {
  const existing = _deployJobs.get(jobId);
  if (existing) return existing;
  const startedAt = Date.now();
  const job: DeployJob = {
    jobId,
    host: workerFp || "Windows update host",
    startedAt,
    updatedAt: startedAt,
    lines: [],
    status: "running",
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
    windowsUpdate: {
      workerFp,
      manifestUrl: "",
      signatureUrl: "",
      manifestSha256: "",
      publisherSha256: "",
      lastSequence: -1,
      startAccepted: true,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
      durable: false,
      mutationTail: Promise.resolve(),
    },
  };
  _deployJobs.set(jobId, job);
  armWindowsUpdateDeadline(
    job,
    WINDOWS_UPDATE_TIMEOUT_MS,
    `Windows update recovery timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
  );
  emitWindowsUpdateLine(job, "resumed signed Windows update from durable worker journal");
  return job;
}

function createPersistedWindowsUpdateJob(record: PersistedWindowsUpdateJob): DeployJob {
  const existing = _deployJobs.get(record.jobId);
  if (existing) return existing;
  const job: DeployJob = {
    jobId: record.jobId,
    host: record.host,
    startedAt: record.startedAt,
    updatedAt: record.updatedAt,
    completedAt: record.completedAt ?? undefined,
    lines: [...record.lines],
    status: record.status,
    exitCode: record.exitCode,
    error: record.error ?? undefined,
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
    windowsUpdate: {
      workerFp: record.workerFp,
      manifestUrl: record.manifestUrl,
      signatureUrl: record.signatureUrl,
      manifestSha256: record.manifestSha256,
      publisherSha256: record.publisherSha256,
      lastSequence: record.lastSequence,
      startAccepted: record.startAccepted,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
      durable: true,
      mutationTail: Promise.resolve(),
    },
  };
  _deployJobs.set(record.jobId, job);
  if (record.status === "done") {
    _gcJob(record.jobId, record.completedAt);
  } else {
    armWindowsUpdateDeadline(
      job,
      record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS - Date.now(),
      `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
    );
  }
  return job;
}

async function recoverWindowsUpdateJobFromRecord(
  record: PersistedWindowsUpdateJob,
): Promise<DeployJob> {
  const existing = _deployJobs.get(record.jobId);
  if (existing) return existing;
  const job = createPersistedWindowsUpdateJob(record);
  if (job.status !== "running") return job;
  if (record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS <= Date.now()) {
    await finishWindowsUpdate(
      job,
      1,
      `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
      record.startedAt + WINDOWS_UPDATE_TIMEOUT_MS,
    );
    return job;
  }
  emitWindowsUpdateLine(job, "resumed signed Windows update from durable coordinator journal");
  const update = job.windowsUpdate!;
  void issueWindowsUpdateCommand(job, update.startAccepted ? "STATUS" : "START");
  return job;
}

async function recoverWindowsUpdateJobFromJournal(jobId: string): Promise<DeployJob | null> {
  if (process.platform !== "win32") return null;
  // The worker journal imports Windows service-control modules; keep that
  // platform-specific dependency out of non-Windows coordinator startup.
  const {
    DurableWindowsUpdateJournalStore,
    readWindowsUpdateProgressFromJournal,
  } = await import("../../roost-cli/src/windows-update-journal.ts");
  const journal = await new DurableWindowsUpdateJournalStore().load();
  if (!journal || journal.jobId !== jobId) return null;
  const job = createTransientRecoveredWindowsUpdateJob(jobId);
  for (const entry of readWindowsUpdateProgressFromJournal(journal, 0)) {
    await handleWorkerUpdateProgress("", {
      request_id: "coordinator-recovery",
      job_id: journal.jobId,
      sequence: entry.sequence,
      phase: entry.phase,
      message: entry.message,
      terminal: entry.terminal,
      success: entry.success,
      error: entry.error,
    });
  }
  return job;
}

function scheduleWindowsUpdateStatus(job: DeployJob, delayMs = WINDOWS_UPDATE_POLL_MS): void {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running" || update.pollTimer) return;
  update.pollTimer = setTimeout(() => {
    update.pollTimer = null;
    void issueWindowsUpdateCommand(job, update.startAccepted ? "STATUS" : "START");
  }, delayMs);
}

async function issueWindowsUpdateCommand(job: DeployJob, action: "START" | "STATUS"): Promise<void> {
  const update = job.windowsUpdate;
  if (!update || job.status !== "running" || update.inFlight || !update.workerFp) return;
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
    if (action === "START") {
      await enqueueWindowsUpdateMutation(job, async () => {
        if (job.status !== "running" || update.startAccepted) return;
        const updatedAt = Date.now();
        if (update.durable) {
          await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job, {
            updatedAt,
            startAccepted: true,
          }));
        }
        update.startAccepted = true;
        job.updatedAt = updatedAt;
      });
    }
    update.lastTransportError = undefined;
  } catch (error) {
    if (job.status !== "running") return;
    if (error instanceof ConnectError
      && error.code !== Code.Unavailable
      && error.code !== Code.DeadlineExceeded) {
      await finishWindowsUpdate(job, 1, error.rawMessage || error.message);
      return;
    }
    // START is idempotent and transport admission is not acceptance. Until a
    // worker ACK/progress is durably recorded, reconnects continue sending
    // START; only an accepted job switches recovery polling to STATUS.
    const message = error instanceof Error ? error.message : String(error);
    if (update.lastTransportError !== message) {
      update.lastTransportError = message;
      emitWindowsUpdateLine(
        job,
        `worker link or coordinator journal unavailable; waiting for persisted updater progress (${message})`,
      );
    }
  } finally {
    update.inFlight = false;
    scheduleWindowsUpdateStatus(job);
  }
}

let windowsUpdateStartTail = Promise.resolve();

async function withSerializedWindowsUpdateStart<T>(operation: () => Promise<T>): Promise<T> {
  const previous = windowsUpdateStartTail;
  let release!: () => void;
  windowsUpdateStartTail = new Promise<void>((resolve) => {
    release = resolve;
  });
  await previous;
  try {
    return await operation();
  } finally {
    release();
  }
}

function windowsUpdateMatches(
  job: DeployJob,
  options: WindowsUpdateDeployOptions,
): boolean {
  const update = job.windowsUpdate;
  return job.status === "running"
    && update !== undefined
    && update.workerFp === options.workerFp
    && update.manifestUrl === options.manifestUrl
    && update.signatureUrl === options.signatureUrl
    && update.manifestSha256 === options.manifestSha256.toLowerCase()
    && update.publisherSha256 === options.publisherSha256.toLowerCase();
}

async function existingWindowsUpdateFor(
  options: WindowsUpdateDeployOptions,
): Promise<{ matching?: DeployJob; conflicting?: DeployJob }> {
  for (const job of _deployJobs.values()) {
    if (job.status !== "running" || job.windowsUpdate?.workerFp !== options.workerFp) continue;
    return windowsUpdateMatches(job, options) ? { matching: job } : { conflicting: job };
  }

  const directory = join(coordDataDir(), "windows-update-jobs");
  let names: string[];
  try {
    names = await readdir(directory);
  } catch (error) {
    if (error instanceof Error && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw error;
  }
  for (const name of names) {
    const jobId = name.endsWith(".json") ? name.slice(0, -5) : "";
    if (!isDeployJobId(jobId)) continue;
    const loaded = await loadPersistedWindowsUpdate(jobId);
    if (loaded.kind !== "record"
      || loaded.record.status !== "running"
      || loaded.record.workerFp !== options.workerFp) continue;
    const job = await recoverWindowsUpdateJobFromRecord(loaded.record);
    return windowsUpdateMatches(job, options) ? { matching: job } : { conflicting: job };
  }
  return {};
}

export async function startWindowsUpdateDeploy(
  host: string,
  options: WindowsUpdateDeployOptions,
): Promise<DeployStartResult> {
  return await withSerializedWindowsUpdateStart(
    () => startWindowsUpdateDeploySerialized(host, options),
  );
}

async function startWindowsUpdateDeploySerialized(
  host: string,
  options: WindowsUpdateDeployOptions,
): Promise<DeployStartResult> {
  if (!HOST_RE.test(host) || host.length > 253) return { ok: false, error: "invalid host" };
  if (!WINDOWS_WORKER_FP_RE.test(options.workerFp)) {
    return { ok: false, error: "invalid worker fingerprint" };
  }
  if (!/^[a-f0-9]{64}$/i.test(options.manifestSha256)) {
    return { ok: false, error: "invalid manifest digest" };
  }
  if (options.publisherSha256 && !/^[a-f0-9]{64}$/i.test(options.publisherSha256)) {
    return { ok: false, error: "invalid Windows publisher pin" };
  }
  if (!httpsUrlSchema.safeParse(options.manifestUrl).success
    || !httpsUrlSchema.safeParse(options.signatureUrl).success) {
    return { ok: false, error: "Windows update manifest URLs must use HTTPS" };
  }
  let existing: Awaited<ReturnType<typeof existingWindowsUpdateFor>>;
  try {
    existing = await existingWindowsUpdateFor(options);
  } catch (error) {
    return {
      ok: false,
      error: `failed to inspect durable Windows update jobs: ${String(error)}`,
    };
  }
  if (existing.matching) return { ok: true, jobId: existing.matching.jobId };
  if (existing.conflicting) {
    return {
      ok: false,
      error: `Windows worker already has active update job ${existing.conflicting.jobId}`,
    };
  }

  const jobId = crypto.randomUUID();
  const startedAt = Date.now();
  const initialLine = normalizeWindowsUpdateLine(`starting signed Windows update on ${host}`)!;
  const job: DeployJob = {
    jobId,
    host,
    startedAt,
    updatedAt: startedAt,
    lines: [initialLine],
    status: "running",
    bus: new BoundedBus<DeployStreamMsg>(2048),
    gcTimer: null,
    windowsUpdate: {
      workerFp: options.workerFp,
      manifestUrl: options.manifestUrl,
      signatureUrl: options.signatureUrl,
      manifestSha256: options.manifestSha256.toLowerCase(),
      publisherSha256: options.publisherSha256.toLowerCase(),
      lastSequence: -1,
      startAccepted: false,
      inFlight: false,
      pollTimer: null,
      deadlineTimer: null,
      durable: true,
      mutationTail: Promise.resolve(),
    },
  };
  try {
    await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { ok: false, error: `failed to persist Windows update job: ${message}` };
  }
  _deployJobs.set(jobId, job);
  armWindowsUpdateDeadline(
    job,
    WINDOWS_UPDATE_TIMEOUT_MS,
    `Windows update timed out after ${WINDOWS_UPDATE_TIMEOUT_MS / 1000}s`,
  );
  void issueWindowsUpdateCommand(job, "START");
  return { ok: true, jobId };
}

async function jobForWindowsUpdateProgress(
  jobId: string,
  workerFp: string,
): Promise<DeployJob | null> {
  const existing = _deployJobs.get(jobId);
  if (existing) return existing;
  const loaded = await loadPersistedWindowsUpdate(jobId);
  if (loaded.kind === "record") return createPersistedWindowsUpdateJob(loaded.record);
  if (loaded.kind !== "missing") return null;
  return createTransientRecoveredWindowsUpdateJob(jobId, workerFp);
}

export async function handleWorkerUpdateProgress(workerFp: string, progress: {
  request_id: string;
  job_id: string;
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}): Promise<void> {
  if (!isDeployJobId(progress.job_id)
    || (workerFp !== "" && !WINDOWS_WORKER_FP_RE.test(workerFp))
    || !Number.isSafeInteger(progress.sequence)
    || progress.sequence < 0
    || typeof progress.phase !== "string"
    || typeof progress.message !== "string"
    || typeof progress.terminal !== "boolean"
    || typeof progress.success !== "boolean"
    || (progress.error !== undefined && typeof progress.error !== "string")) {
    return;
  }
  let job: DeployJob | null;
  try {
    job = await jobForWindowsUpdateProgress(progress.job_id, workerFp);
  } catch {
    return;
  }
  if (!job) return;
  const update = job.windowsUpdate;
  if (!update) return;
  try {
    await enqueueWindowsUpdateMutation(job, async () => {
      if (job.status !== "running") return;
      if (!update.workerFp && workerFp) {
        update.workerFp = workerFp;
        job.host = workerFp;
      }
      if (update.workerFp !== workerFp && workerFp) return;
      if (progress.sequence <= update.lastSequence) {
        if (update.workerFp) scheduleWindowsUpdateStatus(job);
        return;
      }
      const detail = progress.message || progress.error || progress.phase || "update progress";
      const line = normalizeWindowsUpdateLine(`[${progress.phase || "update"}] ${detail}`)!;
      const lines = linesWithWindowsUpdateLine(job.lines, line);
      const updatedAt = Date.now();
      const terminalError = progress.success
        ? undefined
        : (progress.error || "Windows update failed");
      if (progress.terminal) {
        await finishWindowsUpdateNow(
          job,
          progress.success ? 0 : 1,
          terminalError,
          line,
          { sequence: progress.sequence },
        );
        return;
      }
      if (update.durable) {
        await persistWindowsUpdateRecord(persistedWindowsUpdateRecord(job, {
          updatedAt,
          startAccepted: true,
          lastSequence: progress.sequence,
          lines,
        }));
      }
      update.lastSequence = progress.sequence;
      update.startAccepted = true;
      update.lastTransportError = undefined;
      job.updatedAt = updatedAt;
      job.lines = lines;
      job.bus.publish({ kind: "line", text: line });
      if (update.workerFp) scheduleWindowsUpdateStatus(job);
    });
  } catch (error) {
    if (job.status !== "running") return;
    const message = error instanceof Error ? error.message : String(error);
    emitWindowsUpdateLine(job, `coordinator update journal unavailable; awaiting replay (${message})`);
    if (update.workerFp) scheduleWindowsUpdateStatus(job);
  }
}

export function resumeWindowsUpdateDeploysForWorker(workerFp: string): void {
  if (!WINDOWS_WORKER_FP_RE.test(workerFp)) return;
  for (const job of _deployJobs.values()) {
    const update = job.windowsUpdate;
    if (!update || update.workerFp !== workerFp || job.status !== "running") continue;
    if (update.pollTimer) {
      clearTimeout(update.pollTimer);
      update.pollTimer = null;
    }
    void issueWindowsUpdateCommand(job, update.startAccepted ? "STATUS" : "START");
  }
  void resumePersistedWindowsUpdateDeploysForWorker(workerFp);
}

export async function resumePersistedWindowsUpdateDeploysForWorker(workerFp: string): Promise<void> {
  if (!WINDOWS_WORKER_FP_RE.test(workerFp)) return;
  const directory = join(coordDataDir(), "windows-update-jobs");
  let names: string[];
  try {
    names = await readdir(directory);
  } catch (error) {
    if (error instanceof Error && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT") return;
    signal("deploy.failed", {
      host: workerFp,
      reason: `cannot scan durable Windows update jobs: ${String(error)}`,
      cooldownKey: workerFp,
    });
    return;
  }
  for (const name of names) {
    const jobId = name.endsWith(".json") ? name.slice(0, -5) : "";
    if (!isDeployJobId(jobId) || _deployJobs.has(jobId)) continue;
    const loaded = await loadPersistedWindowsUpdate(jobId);
    if (loaded.kind !== "record"
      || loaded.record.status !== "running"
      || loaded.record.workerFp !== workerFp) continue;
    await recoverWindowsUpdateJobFromRecord(loaded.record);
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
  if (!job) {
    const loaded = await loadPersistedWindowsUpdate(jobId);
    if (loaded.kind === "record") {
      job = await recoverWindowsUpdateJobFromRecord(loaded.record);
    } else if (loaded.kind === "missing") {
      job = await recoverWindowsUpdateJobFromJournal(jobId) ?? undefined;
    }
  }
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

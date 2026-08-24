// Process-wide dedupe of in-flight session spawns keyed by caller-minted
// UUID: exact duplicates join the first caller's promise; caller/parameter
// conflicts reject before any membership change; capacity caps at 1024.
// Ambiguous failures (transport loss, timeout) deliberately RETAIN the
// reservation until durable-open reconciliation resolves it — rejecting
// early here would let one lost rpc-ok turn into a duplicate spawn.
import { Code, ConnectError } from "@connectrpc/connect";

const PENDING_SPAWN_TIMEOUT_MS = 30_000;
const COMPLETED_SPAWN_RETENTION_MS = 30_000;
const MAX_PENDING_SPAWNS = 1_024;

export interface PendingSpawnSignature {
  callerKey: string;
  workerFp: string;
  kind: string;
  folder: string;
  cols?: number;
  rows?: number;
}

export interface PendingSpawnResult {
  sessionId: string;
  channelId: number;
}

interface PendingSpawnEntry {
  signatureKey: string;
  signature: PendingSpawnSignature;
  promise: Promise<PendingSpawnResult>;
  resolve: (result: PendingSpawnResult) => void;
  reject: (error: Error) => void;
  state: "pending" | "resolved";
  timer: ReturnType<typeof setTimeout>;
  ambiguous: boolean;
  durableOpened?: { workerFp: string; channelId: number };
}

export type PendingSpawnReservation =
  | { kind: "new"; promise: Promise<PendingSpawnResult> }
  | { kind: "joined"; promise: Promise<PendingSpawnResult> }
  | { kind: "conflict" }
  | { kind: "capacity" };

const pendingSpawns = new Map<string, PendingSpawnEntry>();

function signatureKey(signature: PendingSpawnSignature): string {
  return JSON.stringify({
    callerKey: signature.callerKey,
    workerFp: signature.workerFp,
    kind: signature.kind,
    folder: signature.folder,
    cols: signature.cols ?? null,
    rows: signature.rows ?? null,
  });
}

/** Atomically reserve a caller-minted UUID. Exact duplicate calls share one
 * result; any caller/parameter mismatch is rejected before membership changes. */
export function reservePendingSpawn(
  sessionId: string,
  signature: PendingSpawnSignature,
  timeoutMs = PENDING_SPAWN_TIMEOUT_MS,
): PendingSpawnReservation {
  const key = signatureKey(signature);
  const existing = pendingSpawns.get(sessionId);
  if (existing) {
    return existing.signatureKey === key
      ? { kind: "joined", promise: existing.promise }
      : { kind: "conflict" };
  }
  if (pendingSpawns.size >= MAX_PENDING_SPAWNS) return { kind: "capacity" };

  const { promise, resolve, reject } = Promise.withResolvers<PendingSpawnResult>();
  const entry: PendingSpawnEntry = {
    signatureKey: key,
    signature,
    promise,
    resolve,
    reject,
    state: "pending",
    timer: undefined as unknown as ReturnType<typeof setTimeout>,
    ambiguous: false,
  };
  entry.timer = setTimeout(() => {
    if (pendingSpawns.get(sessionId) !== entry || entry.state !== "pending") return;
    pendingSpawns.delete(sessionId);
    entry.reject(new ConnectError(
      `spawn ${sessionId} did not durably open within ${timeoutMs}ms`,
      Code.DeadlineExceeded,
    ));
  }, timeoutMs);
  entry.timer.unref?.();
  pendingSpawns.set(sessionId, entry);
  return { kind: "new", promise };
}


export function resolvePendingSpawn(
  sessionId: string,
  result: PendingSpawnResult,
): boolean {
  const entry = pendingSpawns.get(sessionId);
  if (!entry || entry.state !== "pending") return false;
  clearTimeout(entry.timer);
  entry.state = "resolved";
  entry.resolve(result);
  entry.timer = setTimeout(() => {
    if (pendingSpawns.get(sessionId) === entry) pendingSpawns.delete(sessionId);
  }, COMPLETED_SPAWN_RETENTION_MS);
  entry.timer.unref?.();
  return true;
}

function resolveFromDurableOpened(
  sessionId: string,
  entry: PendingSpawnEntry,
): boolean {
  const opened = entry.durableOpened;
  if (!opened || !entry.ambiguous || entry.state !== "pending") return false;
  return resolvePendingSpawn(sessionId, {
    sessionId,
    channelId: opened.channelId,
  });
}

/** Record a durable opened event. Normal success still waits for worker rpc-ok,
 * which is ordered after the first full frame. Opened resolves only a lost or
 * otherwise ambiguous worker reply. */
export function resolvePendingSpawnOpened(
  workerFp: string,
  sessionId: string,
  channelId: number,
): boolean {
  const entry = pendingSpawns.get(sessionId);
  if (
    !entry
    || entry.state !== "pending"
    || entry.signature.workerFp !== workerFp
  ) return false;
  entry.durableOpened = { workerFp, channelId };
  resolveFromDurableOpened(sessionId, entry);
  return true;
}

/** Transport loss/timeouts are ambiguous and retain the reservation for
 * durable-open reconciliation; definite failures reject immediately. */
export function rejectPendingSpawn(
  sessionId: string,
  error: Error,
  definite: boolean,
): boolean {
  const entry = pendingSpawns.get(sessionId);
  if (!entry || entry.state !== "pending") return false;
  if (!definite) {
    entry.ambiguous = true;
    resolveFromDurableOpened(sessionId, entry);
    return true;
  }
  clearTimeout(entry.timer);
  pendingSpawns.delete(sessionId);
  entry.reject(error);
  return true;
}

export function pendingSpawnStats(): { pending: number; resolved: number } {
  let pending = 0;
  let resolved = 0;
  for (const entry of pendingSpawns.values()) {
    if (entry.state === "pending") pending++;
    else resolved++;
  }
  return { pending, resolved };
}

export function resetPendingSpawnsForTest(): void {
  for (const entry of pendingSpawns.values()) clearTimeout(entry.timer);
  pendingSpawns.clear();
}

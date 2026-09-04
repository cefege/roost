// Binds authoritative SessionsList snapshots to the live Sync v2 socket that
// requested them: bind issues a token, terminal domain_ready consumes it
// exactly once. Tokens are capped at 4 per socket (oldest evicted), and the
// unregister disposer is identity-safe so a late double-close can never
// delete a re-registered socket's binding.
import { randomUUID } from "node:crypto";

interface SnapshotBinding {
  readonly fingerprint: string;
  readonly dashboardId: string;
  readonly tokens: Map<string, ReadonlySet<string>>;
}

const sockets = new Map<string, SnapshotBinding>();
const MAX_TOKENS_PER_SOCKET = 4;

/** Register one live Sync v2 socket. The returned disposer is identity-safe. */
export function registerSyncSnapshotSocket(
  socketId: string,
  fingerprint: string,
  dashboardId: string,
): () => void {
  const binding: SnapshotBinding = { fingerprint, dashboardId, tokens: new Map() };
  sockets.set(socketId, binding);
  return () => {
    if (sockets.get(socketId) === binding) sockets.delete(socketId);
    binding.tokens.clear();
  };
}

/** Bind an authoritative SessionsList result to its requesting live socket. */
export function bindSyncSessionSnapshot(
  socketId: string,
  fingerprint: string,
  dashboardId: string,
  sessionIds: Iterable<string>,
): string | null {
  const binding = sockets.get(socketId);
  if (
    !binding
    || binding.fingerprint !== fingerprint
    || binding.dashboardId !== dashboardId
  ) return null;
  const token = randomUUID();
  binding.tokens.set(token, new Set(sessionIds));
  while (binding.tokens.size > MAX_TOKENS_PER_SOCKET) {
    const oldest = binding.tokens.keys().next().value as string | undefined;
    if (!oldest) break;
    binding.tokens.delete(oldest);
  }
  return token;
}

/** Consume a token exactly once when terminal domain_ready arrives. */
export function consumeSyncSessionSnapshot(
  socketId: string,
  token: string,
): ReadonlySet<string> | null {
  const binding = sockets.get(socketId);
  if (!binding) return null;
  const sessionIds = binding.tokens.get(token);
  if (!sessionIds) return null;
  binding.tokens.clear();
  return sessionIds;
}

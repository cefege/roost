// Shared stub for the reconnect-respawn tests: the Kysely query chain
// respawnMissingForWorker walks, resolving to exactly one open shell session.
// Used by worker-respawn.test.ts (write-gate deferral) and
// worker-respawn-geometry.test.ts (dispatched geometry) so the query shape
// lives once.

const DEFAULT_SESSION_ID = "00000000-0000-4000-8000-000000000001";

export function databaseWithOpenSession(sessionId: string = DEFAULT_SESSION_ID) {
  const query = {
    innerJoin: () => query,
    select: () => query,
    where: () => query,
    execute: async () => [{ id: sessionId, kind: "shell", cwd: "/tmp" }],
  };
  return { selectFrom: () => query };
}

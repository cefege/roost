// Resolves persisted worker and session authority before an event append.
// Worker-originated resource probes fail as a data outcome, not an exception,
// so missing and foreign IDs receive neither an ACK nor a socket-close oracle.
// Event transactions call this inside their SQLite transaction.

import type { SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "./db/connection.ts";

export interface EventAdmissionOptions {
  worker_fp: string | null;
  client_seq: number | null;
}

export interface EventAdmission {
  admitted: boolean;
  sessionId: string | null;
  sessionExists: boolean;
}

function rejected(sessionId: string | null): EventAdmission {
  return { admitted: false, sessionId, sessionExists: false };
}

export async function resolveEventAdmission(
  db: KyselyDB,
  event: SessionEvent,
  options: EventAdmissionOptions,
): Promise<EventAdmission> {
  const sessionId = "session_id" in event ? event.session_id : null;
  if (options.worker_fp === null) {
    const sessionExists = sessionId === null
      ? false
      : await db.selectFrom("sessions")
        .select("id")
        .where("id", "=", sessionId)
        .executeTakeFirst()
        .then((row) => row !== undefined);
    return { admitted: true, sessionId, sessionExists };
  }

  const worker = await db.selectFrom("workers")
    .select("fp")
    .where("fp", "=", options.worker_fp)
    .where("deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (!worker) return rejected(sessionId);

  if (
    (event.kind === "opened" || event.kind === "snapshot")
    && event.worker_fp !== options.worker_fp
  ) return rejected(sessionId);

  if (options.client_seq !== null) {
    const durableDelivery = await db.selectFrom("events")
      .select("id")
      .where("worker_fp", "=", options.worker_fp)
      .where("client_seq", "=", options.client_seq)
      .executeTakeFirst();
    if (durableDelivery) {
      return { admitted: true, sessionId, sessionExists: false };
    }
  }

  if (event.kind === "snapshot") {
    if (event.sessions.some((session) => session.worker_fp !== options.worker_fp)) {
      return rejected(sessionId);
    }
    const announcedIds = [...new Set(event.sessions.map((session) => session.id))];
    const currentRows = announcedIds.length === 0
      ? []
      : await db.selectFrom("sessions")
        .select(["id", "worker_fp"])
        .where("id", "in", announcedIds)
        .execute();
    if (currentRows.some((row) => row.worker_fp !== options.worker_fp)) {
      return rejected(sessionId);
    }

    const workspaceIds = [...new Set(
      event.sessions.flatMap((session) =>
        session.workspace_id === null ? [] : [session.workspace_id]
      ),
    )];
    if (workspaceIds.length > 0) {
      const workspaceRows = await db.selectFrom("workspaces")
        .select("id")
        .where("id", "in", workspaceIds)
        .execute();
      if (workspaceRows.length !== workspaceIds.length) {
        return rejected(sessionId);
      }
    }
    return { admitted: true, sessionId, sessionExists: false };
  }

  if (sessionId === null) {
    return { admitted: true, sessionId, sessionExists: false };
  }
  const existingSession = await db.selectFrom("sessions")
    .select(["id", "worker_fp"])
    .where("id", "=", sessionId)
    .executeTakeFirst();
  if (existingSession) {
    return existingSession.worker_fp === options.worker_fp
      ? { admitted: true, sessionId, sessionExists: true }
      : rejected(sessionId);
  }
  if (event.kind === "agent_reference") {
    const priorOwnedSession = await db.selectFrom("events")
      .select("id")
      .where("session_id", "=", sessionId)
      .where("worker_fp", "=", options.worker_fp)
      .where("kind", "=", "opened")
      .executeTakeFirst();
    if (priorOwnedSession) {
      // A reference queued before an offline force-close must still be consumed
      // or it permanently blocks the worker's ordered durable replay.
      return { admitted: true, sessionId, sessionExists: false };
    }
  }
  if (event.kind !== "opened") return rejected(sessionId);
  return { admitted: true, sessionId, sessionExists: false };
}

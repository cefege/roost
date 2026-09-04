// Resolves persisted dashboard and session authority before an event append.
// Worker-originated resource probes fail as a data outcome, not an exception,
// so missing and foreign IDs receive neither an ACK nor a socket-close oracle.
// Event transactions call this inside their SQLite transaction.

import type { SessionEvent } from "@roost/shared/wire";
import type { KyselyDB } from "./db/connection.ts";

export interface EventAdmissionOptions {
  worker_fp: string | null;
  client_seq: number | null;
  dashboardId?: string;
  requireExistingWorkerSessions?: boolean;
  allowNewWorkerSession?: (
    dashboardId: string,
    workerFp: string,
    sessionId: string,
  ) => boolean;
}

export interface EventAdmission {
  admitted: boolean;
  dashboardId: string | null;
  sessionId: string | null;
  sessionExists: boolean;
}

function rejected(
  dashboardId: string | null,
  sessionId: string | null,
): EventAdmission {
  return { admitted: false, dashboardId, sessionId, sessionExists: false };
}

export async function resolveEventAdmission(
  db: KyselyDB,
  event: SessionEvent,
  options: EventAdmissionOptions,
): Promise<EventAdmission> {
  const sessionId = "session_id" in event ? event.session_id : null;
  if (options.worker_fp === null) {
    let dashboardId = options.dashboardId ?? null;
    if (dashboardId === null && sessionId !== null) {
      dashboardId = (await db.selectFrom("sessions")
        .select("dashboard_id")
        .where("id", "=", sessionId)
        .executeTakeFirst())?.dashboard_id ?? null;
    }
    if (dashboardId === null) {
      throw new Error("event has no persisted dashboard scope");
    }
    const sessionExists = sessionId === null
      ? false
      : await db.selectFrom("sessions")
        .select("id")
        .where("id", "=", sessionId)
        .where("dashboard_id", "=", dashboardId)
        .executeTakeFirst()
        .then((row) => row !== undefined);
    return { admitted: true, dashboardId, sessionId, sessionExists };
  }

  const worker = await db.selectFrom("workers as worker")
    .innerJoin("dashboards as dashboard", "dashboard.id", "worker.dashboard_id")
    .innerJoin(
      "organizations as organization",
      "organization.id",
      "dashboard.organization_id",
    )
    .select("worker.dashboard_id as dashboardId")
    .where("worker.fp", "=", options.worker_fp)
    .where("worker.deleted_at_ms", "is", null)
    .where("dashboard.status", "=", "active")
    .where("organization.status", "=", "active")
    .executeTakeFirst();
  const dashboardId = worker?.dashboardId ?? null;
  if (
    dashboardId === null
    || (options.dashboardId !== undefined && options.dashboardId !== dashboardId)
  ) return rejected(dashboardId, sessionId);

  if (
    (event.kind === "opened" || event.kind === "snapshot")
    && event.worker_fp !== options.worker_fp
  ) return rejected(dashboardId, sessionId);

  if (options.client_seq !== null) {
    const durableDelivery = await db.selectFrom("events")
      .select("id")
      .where("dashboard_id", "=", dashboardId)
      .where("worker_fp", "=", options.worker_fp)
      .where("client_seq", "=", options.client_seq)
      .executeTakeFirst();
    if (durableDelivery) {
      return { admitted: true, dashboardId, sessionId, sessionExists: false };
    }
  }

  if (event.kind === "snapshot") {
    if (event.sessions.some((session) => session.worker_fp !== options.worker_fp)) {
      return rejected(dashboardId, sessionId);
    }
    const workspaceIds = [...new Set(
      event.sessions.flatMap((session) =>
        session.workspace_id === null ? [] : [session.workspace_id]
      ),
    )];
    if (workspaceIds.length > 0) {
      const workspaceRows = await db.selectFrom("workspaces")
        .select("id")
        .where("dashboard_id", "=", dashboardId)
        .where("id", "in", workspaceIds)
        .execute();
      if (workspaceRows.length !== workspaceIds.length) {
        return rejected(dashboardId, sessionId);
      }
    }
    if (!options.requireExistingWorkerSessions || event.sessions.length === 0) {
      return { admitted: true, dashboardId, sessionId, sessionExists: false };
    }
    const announcedIds = [...new Set(event.sessions.map((session) => session.id))];
    const currentRows = await db.selectFrom("sessions")
      .select("id")
      .where("dashboard_id", "=", dashboardId)
      .where("worker_fp", "=", options.worker_fp)
      .where("id", "in", announcedIds)
      .execute();
    const provenIds = new Set(currentRows.map((row) => row.id));
    const missingIds = announcedIds.filter((id) => !provenIds.has(id));
    if (missingIds.length > 0) {
      const openedRows = await db.selectFrom("events")
        .select("session_id")
        .where("dashboard_id", "=", dashboardId)
        .where("worker_fp", "=", options.worker_fp)
        .where("kind", "=", "opened")
        .where("session_id", "in", missingIds)
        .execute();
      for (const row of openedRows) {
        if (row.session_id !== null) provenIds.add(row.session_id);
      }
    }
    return provenIds.size === announcedIds.length
      ? { admitted: true, dashboardId, sessionId, sessionExists: false }
      : rejected(dashboardId, sessionId);
  }

  if (sessionId === null) {
    return { admitted: true, dashboardId, sessionId, sessionExists: false };
  }
  const existingSession = await db.selectFrom("sessions")
    .select("id")
    .where("id", "=", sessionId)
    .where("dashboard_id", "=", dashboardId)
    .where("worker_fp", "=", options.worker_fp)
    .executeTakeFirst();
  if (existingSession) {
    return { admitted: true, dashboardId, sessionId, sessionExists: true };
  }
  if (event.kind !== "opened") return rejected(dashboardId, sessionId);
  if (
    options.allowNewWorkerSession
    && !options.allowNewWorkerSession(dashboardId, options.worker_fp, sessionId)
  ) return rejected(dashboardId, sessionId);
  return { admitted: true, dashboardId, sessionId, sessionExists: false };
}

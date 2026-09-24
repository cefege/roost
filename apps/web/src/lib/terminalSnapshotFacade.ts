// Terminal production snapshot facade — main.tsx installs one content-free window reader.
// It validates a terminal UUID before its bounded coordinator RPC and sends only the explicit pick.
// terminalDiagSnapshot.ts owns browser measurement; this module owns production exposure and JSON decoding.
// It is also the production entry point for opt-in incident capture: the menu and
// the automatic recorder hooks call the three capture APIs re-exported below, and
// nothing capture-related is installed on `window` — no generic command evaluator exists.

import { isTerminalUuid } from "@roost/protocol/viewport";
import { coordClient } from "../connect.ts";
import {
  terminalBrowserStreamSnapshot,
  type TerminalBrowserStreamSnapshot,
} from "./terminalDiagSnapshot.ts";

export interface TerminalProductionSnapshot {
  session_id: TerminalBrowserStreamSnapshot["session_id"];
  captured_at_ms: TerminalBrowserStreamSnapshot["captured_at_ms"];
  build: TerminalBrowserStreamSnapshot["build"];
  wire_received: TerminalBrowserStreamSnapshot["wire_received"];
  replica: TerminalBrowserStreamSnapshot["replica"];
  view: TerminalBrowserStreamSnapshot["view"];
  sync: TerminalBrowserStreamSnapshot["sync"];
  route: TerminalBrowserStreamSnapshot["route"];
  handler_canonical: TerminalBrowserStreamSnapshot["handler_canonical"];
  dom_reconciled: TerminalBrowserStreamSnapshot["dom_reconciled"];
  reconcile_block_reason: TerminalBrowserStreamSnapshot["reconcile_block_reason"];
  presentation: TerminalBrowserStreamSnapshot["presentation"];
  slot: TerminalBrowserStreamSnapshot["slot"];
  visibility: TerminalBrowserStreamSnapshot["visibility"];
}

export interface TerminalProductionSnapshotResult {
  browser: TerminalProductionSnapshot;
  diagnostic: unknown;
}

type TerminalSnapshotWindow = Window & {
  __roostTerminalSnapshot?: (sessionId: string) => Promise<TerminalProductionSnapshotResult>;
};

export function installTerminalSnapshotFacade(): void {
  if (typeof window === "undefined") return;
  (window as TerminalSnapshotWindow).__roostTerminalSnapshot = readTerminalProductionSnapshot;
}

/** Capture control. The recorder owns lease state, evidence freezing and the
 *  authenticated RPC; these names exist so the menu and the automatic detectors
 *  share one production entry point instead of importing the recorder ad hoc. */
export {
  captureTerminalIncident,
  startTerminalCapture,
  stopTerminalCapture,
} from "./terminalIncidentCapture.ts";

async function readTerminalProductionSnapshot(
  sessionId: string,
): Promise<TerminalProductionSnapshotResult> {
  if (typeof sessionId !== "string" || !isTerminalUuid(sessionId)) {
    throw new Error(`invalid terminal snapshot session id: ${String(sessionId)}`);
  }
  const browser = productionSnapshotFor(sessionId);
  const response = await coordClient.diagSnapshot({
    sessionFilterIds: [sessionId],
    spaStateJson: JSON.stringify(browser),
  });
  const snapshotJson = response?.snapshotJson;
  if (typeof snapshotJson !== "string" || snapshotJson.trim().length === 0) {
    throw new Error("terminal diagnostic RPC returned empty snapshotJson");
  }
  try {
    return { browser, diagnostic: JSON.parse(snapshotJson) };
  } catch (error) {
    throw new Error(`terminal diagnostic RPC returned invalid snapshotJson: ${String(error)}`);
  }
}

function productionRouteEntry(
  route: TerminalBrowserStreamSnapshot["route"]["active"],
) {
  if (!route) return null;
  return {
    kind: route.kind,
    worker_epoch: route.worker_epoch,
    peer_id: route.peer_id,
    phase: route.phase,
    candidate_type: route.candidate_type,
    probe_age_ms: route.probe_age_ms,
    rtt_ms: route.rtt_ms,
    worker_control_rtt_ms: route.worker_control_rtt_ms,
    buffered_bytes: route.buffered_bytes,
  };
}

function productionSnapshotFor(sessionId: string): TerminalProductionSnapshot {
  const snapshot = terminalBrowserStreamSnapshot(sessionId);
  const route = snapshot.route;
  return {
    session_id: snapshot.session_id,
    captured_at_ms: snapshot.captured_at_ms,
    build: snapshot.build,
    wire_received: snapshot.wire_received,
    replica: snapshot.replica,
    view: snapshot.view,
    sync: snapshot.sync,
    route: {
      active: productionRouteEntry(route.active),
      candidate: productionRouteEntry(route.candidate),
      peer_phase: route.peer_phase,
      fallback_reason: route.fallback_reason,
      failure_detail: route.failure_detail,
      input_phase: route.input_phase,
      pending_input_count: route.pending_input_count,
    },
    handler_canonical: snapshot.handler_canonical,
    dom_reconciled: snapshot.dom_reconciled,
    reconcile_block_reason: snapshot.reconcile_block_reason,
    presentation: snapshot.presentation,
    slot: snapshot.slot,
    visibility: snapshot.visibility,
  };
}

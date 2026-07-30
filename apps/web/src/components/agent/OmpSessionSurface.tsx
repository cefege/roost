/** @jsxImportSource react */
import { createRoot, type Root } from "react-dom/client";
import { useSyncExternalStore } from "react";
import {
  SessionReplica,
  SessionSurface,
  type SessionActions,
  type TranscriptResult,
} from "@oh-my-pi/collab-web";
import type { CollabUiResponseValue, HostFrame } from "@oh-my-pi/pi-wire";
import { coordClient } from "../../connect.ts";

interface ReplicaRecord {
  replica: SessionReplica;
  loaded: boolean;
  loading: Promise<void> | null;
  pending: Array<{ revision: bigint; frame: HostFrame }>;
  revision: bigint;
}

const records = new Map<string, ReplicaRecord>();

function recordFor(sessionId: string): ReplicaRecord {
  let record = records.get(sessionId);
  if (!record) {
    record = { replica: new SessionReplica(), loaded: false, loading: null, pending: [], revision: 0n };
    records.set(sessionId, record);
  }
  return record;
}

function decodeFrame(frameJson: string): HostFrame {
  const value: unknown = JSON.parse(frameJson);
  if (!value || typeof value !== "object" || !("t" in value) || typeof value.t !== "string") {
    throw new Error("invalid OMP HostFrame");
  }
  return value as HostFrame;
}

export function applyAgentUiFrame(sessionId: string, frameJson: string, revision: bigint): void {
  const record = recordFor(sessionId);
  if (revision <= record.revision) return;
  if (record.loaded && revision > record.revision + 1n) {
    record.loaded = false;
    record.pending.push({ revision, frame: decodeFrame(frameJson) });
    if (!record.loading) record.loading = loadSnapshot(sessionId, record);
    return;
  }
  const frame = decodeFrame(frameJson);
  if (!record.loaded) {
    record.pending.push({ revision, frame });
    if (!record.loading) record.loading = loadSnapshot(sessionId, record);
    return;
  }
  record.revision = revision;
  record.replica.applyFrame(frame);
}

async function loadSnapshot(sessionId: string, record: ReplicaRecord): Promise<void> {
  try {
    let sawWelcome = false;
    let sawFinalSnapshotChunk = false;
    for await (const response of coordClient.sessionsGetAgentUiSnapshot({ sessionId })) {
      const frame = response.frame;
      if (!frame?.frameJson || frame.coordRevision <= record.revision) continue;
      const decoded = decodeFrame(frame.frameJson);
      if (decoded.t === "welcome") sawWelcome = true;
      if (decoded.t === "snapshot-chunk" && decoded.final) sawFinalSnapshotChunk = true;
      record.replica.applyFrame(decoded);
      record.revision = frame.coordRevision;
    }
    if (!sawWelcome || !sawFinalSnapshotChunk) throw new Error("OMP snapshot is not ready");
    record.pending.sort((a, b) => a.revision < b.revision ? -1 : a.revision > b.revision ? 1 : 0);
    const firstPendingRevision = record.pending.find(pending => pending.revision > record.revision)?.revision;
    if (firstPendingRevision !== undefined && firstPendingRevision > record.revision + 1n) {
      throw new Error("OMP snapshot changed during replay");
    }
    for (const pending of record.pending) {
      if (pending.revision <= record.revision) continue;
      if (pending.revision > record.revision + 1n) throw new Error("OMP live frame revision gap");
      record.replica.applyFrame(pending.frame);
      record.revision = pending.revision;
    }
    record.pending.length = 0;
    record.loaded = true;
    record.replica.setPhase("live");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    record.replica.setPhase("reconnecting", message);
    setTimeout(() => {
      if (!record.loaded && !record.loading) record.loading = loadSnapshot(sessionId, record);
    }, 2_000);
  } finally {
    record.loading = null;
  }
}

function ensureSnapshot(sessionId: string): ReplicaRecord {
  const record = recordFor(sessionId);
  if (!record.loaded && !record.loading) record.loading = loadSnapshot(sessionId, record);
  return record;
}

async function command(sessionId: string, value: Record<string, unknown>): Promise<unknown> {
  const response = await coordClient.sessionsAgentUiCommand({ sessionId, commandJson: JSON.stringify(value) });
  if (!response.accepted) throw new Error("OMP rejected the browser command");
  return response.dataJson ? JSON.parse(response.dataJson) : undefined;
}

class RoostSessionActions implements SessionActions {
  constructor(readonly sessionId: string) {}

  sendPrompt(text: string): void {
    void command(this.sessionId, { type: "prompt", message: text });
  }
  sendUiResponse(reqId: number, value?: CollabUiResponseValue): void {
    void command(this.sessionId, { type: "browser_ui_response", reqId, value, cancelled: value === undefined });
  }

  sendAbort(): void {
    void command(this.sessionId, { type: "abort" });
  }

  sendAgentCmd(cmd: "chat" | "kill" | "revive", agentId: string, text?: string): void {
    void command(this.sessionId, { type: "subagent_command", command: cmd, agentId, text });
  }

  async fetchTranscript(agentId: string, fromByte: number): Promise<TranscriptResult | null> {
    try {
      const data = await command(this.sessionId, { type: "get_subagent_messages", subagentId: agentId, fromByte }) as {
        nextByte?: number;
        entries?: unknown[];
        messages?: unknown[];
      } | undefined;
      if (!data) return { kind: "error", message: "OMP returned no transcript" };
      const rows = data.entries ?? data.messages ?? [];
      return { kind: "rows", text: rows.map(row => JSON.stringify(row)).join("\n"), newSize: data.nextByte ?? fromByte };
    } catch (error) {
      return { kind: "error", message: error instanceof Error ? error.message : String(error) };
    }
  }
}

function Surface({ sessionId }: { sessionId: string }) {
  const record = ensureSnapshot(sessionId);
  const snapshot = useSyncExternalStore(record.replica.subscribe, record.replica.getSnapshot);
  return <SessionSurface snapshot={snapshot} actions={new RoostSessionActions(sessionId)} />;
}

export function mountOmpSessionSurface(element: HTMLElement, sessionId: string): Root {
  const root = createRoot(element);
  root.render(<Surface sessionId={sessionId} />);
  return root;
}

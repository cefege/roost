// Durable, browser-facing OMP HostFrame replica.
//
// Welcome/snapshot-chunk trains are reconciliation state: coord stages them
// without a revision or firehose publish. On the final chunk, one transaction
// assigns snapshot revisions first, then revisions any live frames that arrived
// mid-train, commits both, and returns only those true-live frames for relay.
// Thus replay and firehose share one monotonic cursor and an incomplete train
// can never replace the prior durable replica.

import { Database } from "bun:sqlite";
import type { AgentUiFrame } from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";

export const AGENT_UI_FRAME_MAX_BYTES = 68 * 1024 * 1024;
const SNAPSHOT_MAX_BYTES = 1024 * 1024 * 1024;
const SNAPSHOT_WIRE_MAX_BYTES = 1152 * 1024 * 1024;
const LIVE_STAGING_MAX_BYTES = AGENT_UI_FRAME_MAX_BYTES;
const SNAPSHOT_MAX_ENTRIES = 100_000;
const SNAPSHOT_ID_MAX_BYTES = 256;
const SESSION_ID_MAX_BYTES = 256;
const ENTRY_ID_MAX_BYTES = 1024;
const STAGING_TTL_MS = 60 * 60 * 1000;
const STAGING_SWEEP_INTERVAL_MS = 15 * 60 * 1000;

const SNAPSHOT_CHUNK_PREFIX = '{"t":"snapshot-chunk","entries":[';
const SNAPSHOT_CHUNK_SUFFIX = '],"final":false}';

interface JsonRecord {
  [key: string]: unknown;
}

export interface ValidatedAgentUiFrame {
  readonly sessionId: string;
  readonly frameJson: string;
  readonly snapshotId: string;
  readonly hostFrame: JsonRecord;
}

export interface StoredAgentUiFrame {
  readonly frame_json: string;
  readonly snapshot_id: string;
  readonly coord_revision: number;
}

export interface RevisionedAgentUiRelay extends StoredAgentUiFrame {
  readonly session_id: string;
}

export type AgentUiIngestResult =
  | "snapshot-started"
  | "snapshot-staged"
  | "snapshot-committed"
  | "snapshot-incomplete"
  | "live-staged"
  | "live-persisted"
  | "relay-only";

export interface AgentUiIngestOutcome {
  readonly result: AgentUiIngestResult;
  /** Highest revision assigned by this ingest; zero while only staging. */
  readonly coord_revision: number;
  /** True-live frames to publish after the surrounding SQLite transaction commits. */
  readonly relays: readonly RevisionedAgentUiRelay[];
}

export class AgentUiProtocolError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AgentUiProtocolError";
  }
}

interface StagingRow {
  snapshot_id: string;
  baseline_revision: number;
  welcome_json: string;
  state_json: string;
  agents_json: string;
  expected_entries: number;
  staged_bytes: number;
  staged_frame_bytes: number;
  staged_live_bytes: number;
}

interface ReplayMetaRow {
  snapshot_id: string | null;
}

interface ReplayRow {
  revision: number;
  frame_json: string;
  is_snapshot: number;
}

interface MaterializedReplay {
  readonly db: Database;
  readonly snapshotId: string;
  readonly rows: readonly ReplayRow[] | null;
  readonly close: () => void;
}

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function byteLength(value: string): number {
  return Buffer.byteLength(value, "utf8");
}

function requireBoundedString(
  value: unknown,
  name: string,
  maxBytes: number,
  allowEmpty = false,
): string {
  if (typeof value !== "string" || (!allowEmpty && value.length === 0)) {
    throw new AgentUiProtocolError(`${name} must be ${allowEmpty ? "a" : "a nonempty"} string`);
  }
  if (byteLength(value) > maxBytes) {
    throw new AgentUiProtocolError(`${name} exceeds ${maxBytes} UTF-8 bytes`);
  }
  return value;
}

function requireEntry(value: unknown, context: string): JsonRecord & { id: string } {
  if (!isRecord(value)) throw new AgentUiProtocolError(`${context} must be an object`);
  requireBoundedString(value.id, `${context}.id`, ENTRY_ID_MAX_BYTES);
  return value as JsonRecord & { id: string };
}

function requireReplayableEntryJson(entryJson: string, context: string): void {
  const snapshotBytes = byteLength(SNAPSHOT_CHUNK_PREFIX)
    + byteLength(entryJson)
    + byteLength(SNAPSHOT_CHUNK_SUFFIX);
  if (snapshotBytes > AGENT_UI_FRAME_MAX_BYTES) {
    throw new AgentUiProtocolError(`${context} exceeds the snapshot replay limit`);
  }
}

function requireWelcome(frame: JsonRecord): void {
  if (!Number.isInteger(frame.proto) || (frame.proto as number) < 1) {
    throw new AgentUiProtocolError("welcome.proto must be a positive integer");
  }
  if (!isRecord(frame.header)) throw new AgentUiProtocolError("welcome.header must be an object");
  if (!isRecord(frame.state)) throw new AgentUiProtocolError("welcome.state must be an object");
  if (!Array.isArray(frame.agents)) throw new AgentUiProtocolError("welcome.agents must be an array");
  if (!Number.isInteger(frame.entryCount)
    || (frame.entryCount as number) < 0
    || (frame.entryCount as number) > SNAPSHOT_MAX_ENTRIES) {
    throw new AgentUiProtocolError(`welcome.entryCount must be between 0 and ${SNAPSHOT_MAX_ENTRIES}`);
  }
}

function requireSnapshotChunk(frame: JsonRecord): void {
  if (!Array.isArray(frame.entries)) {
    throw new AgentUiProtocolError("snapshot-chunk.entries must be an array");
  }
  if (typeof frame.final !== "boolean") {
    throw new AgentUiProtocolError("snapshot-chunk.final must be a boolean");
  }
  for (let i = 0; i < frame.entries.length; i++) {
    const entry = requireEntry(frame.entries[i], `snapshot-chunk.entries[${i}]`);
    requireReplayableEntryJson(JSON.stringify(entry), `snapshot-chunk.entries[${i}]`);
  }
}

/** Validate the transport envelope and the HostFrame fields coord persists. */
export function validateAgentUiFrame(
  frame: Pick<AgentUiFrame, "sessionId" | "frameJson" | "snapshotId">,
): ValidatedAgentUiFrame {
  const sessionId = requireBoundedString(frame.sessionId, "session_id", SESSION_ID_MAX_BYTES);
  const frameJson = requireBoundedString(frame.frameJson, "frame_json", AGENT_UI_FRAME_MAX_BYTES);
  const snapshotId = requireBoundedString(
    frame.snapshotId,
    "snapshot_id",
    SNAPSHOT_ID_MAX_BYTES,
    true,
  );

  let decoded: unknown;
  try {
    decoded = JSON.parse(frameJson);
  } catch {
    throw new AgentUiProtocolError("frame_json is not valid JSON");
  }
  if (!isRecord(decoded) || typeof decoded.t !== "string" || decoded.t.length === 0) {
    throw new AgentUiProtocolError("frame_json must contain one HostFrame object");
  }

  if (snapshotId) {
    if (decoded.t === "welcome") requireWelcome(decoded);
    else if (decoded.t === "snapshot-chunk") requireSnapshotChunk(decoded);
    else throw new AgentUiProtocolError("snapshot_id is only valid on welcome and snapshot-chunk frames");
  } else {
    if (decoded.t === "welcome" || decoded.t === "snapshot-chunk") {
      throw new AgentUiProtocolError(`${decoded.t} requires a nonempty snapshot_id`);
    }
    if (decoded.t === "entry") {
      const entry = requireEntry(decoded.entry, "entry.entry");
      requireReplayableEntryJson(JSON.stringify(entry), "entry.entry");
    }
    if (decoded.t === "state" && !isRecord(decoded.state)) {
      throw new AgentUiProtocolError("state.state must be an object");
    }
    if (decoded.t === "agents" && !Array.isArray(decoded.agents)) {
      throw new AgentUiProtocolError("agents.agents must be an array");
    }
  }

  return { sessionId, frameJson, snapshotId, hostFrame: decoded };
}

function clearStaging(sqlite: Database, sessionId: string): void {
  sqlite.query("DELETE FROM agent_ui_live_frame_staging WHERE session_id = ?").run(sessionId);
  sqlite.query("DELETE FROM agent_ui_snapshot_frame_staging WHERE session_id = ?").run(sessionId);
  sqlite.query("DELETE FROM agent_ui_snapshot_entries WHERE session_id = ?").run(sessionId);
  sqlite.query("DELETE FROM agent_ui_snapshot_staging WHERE session_id = ?").run(sessionId);
}

function ensureSessionRow(sqlite: Database, sessionId: string, now: number): void {
  sqlite.query(
    `INSERT INTO agent_ui_sessions
       (session_id, snapshot_id, welcome_json, state_json, agents_json, last_revision, updated_at)
     VALUES (?, NULL, NULL, NULL, NULL, 0, ?)
     ON CONFLICT(session_id) DO NOTHING`,
  ).run(sessionId, now);
}

function currentRevision(sqlite: Database, sessionId: string): number {
  const revision = sqlite.query<{ last_revision: number }, [string]>(
    "SELECT last_revision FROM agent_ui_sessions WHERE session_id = ?",
  ).get(sessionId)?.last_revision ?? 0;
  if (!Number.isSafeInteger(revision) || revision < 0) {
    throw new Error("agent UI revision is invalid");
  }
  return revision;
}

function setRevision(sqlite: Database, sessionId: string, revision: number, now: number): void {
  if (!Number.isSafeInteger(revision) || revision < 1) {
    throw new Error("agent UI revision exhausted");
  }
  sqlite.query(
    "UPDATE agent_ui_sessions SET last_revision = ?, updated_at = ? WHERE session_id = ?",
  ).run(revision, now, sessionId);
}

function allocateRevision(sqlite: Database, sessionId: string, now: number): number {
  const revision = currentRevision(sqlite, sessionId) + 1;
  setRevision(sqlite, sessionId, revision, now);
  return revision;
}

function startSnapshot(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  now: number,
): RevisionedAgentUiRelay[] {
  // A new welcome supersedes an incomplete train. Preserve any true-live frames
  // queued behind the old train against the last completed baseline first.
  const abandonedRelays = flushQueuedLive(sqlite, frame.sessionId, now);
  clearStaging(sqlite, frame.sessionId);
  ensureSessionRow(sqlite, frame.sessionId, now);
  const baselineRevision = currentRevision(sqlite, frame.sessionId);
  const welcome = frame.hostFrame;
  sqlite.query(
    `INSERT INTO agent_ui_snapshot_staging
       (session_id, snapshot_id, baseline_revision, welcome_json, state_json, agents_json,
        expected_entries, staged_bytes, staged_frame_bytes, staged_live_bytes,
        created_at, updated_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?, 0, ?, ?)`,
  ).run(
    frame.sessionId,
    frame.snapshotId,
    baselineRevision,
    frame.frameJson,
    JSON.stringify({ t: "state", state: welcome.state }),
    JSON.stringify({ t: "agents", agents: welcome.agents }),
    welcome.entryCount as number,
    byteLength(frame.frameJson),
    now,
    now,
  );
  sqlite.query(
    `INSERT INTO agent_ui_snapshot_frame_staging
       (session_id, ordinal, frame_json) VALUES (?, 0, ?)`,
  ).run(frame.sessionId, frame.frameJson);
  return abandonedRelays;
}
function queueLiveFrame(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  now: number,
): boolean {
  const staging = sqlite.query<{ staged_live_bytes: number }, [string]>(
    "SELECT staged_live_bytes FROM agent_ui_snapshot_staging WHERE session_id = ?",
  ).get(frame.sessionId);
  if (!staging) throw new Error("agent UI live queue has no snapshot staging row");
  const stagedLiveBytes = staging.staged_live_bytes + byteLength(frame.frameJson);
  if (stagedLiveBytes > LIVE_STAGING_MAX_BYTES) {
    return false;
  }
  const ordinal = sqlite.query<{ next_ordinal: number }, [string]>(
    `SELECT COALESCE(MAX(ordinal), -1) + 1 AS next_ordinal
       FROM agent_ui_live_frame_staging WHERE session_id = ?`,
  ).get(frame.sessionId)?.next_ordinal ?? 0;
  sqlite.query(
    `INSERT INTO agent_ui_live_frame_staging (session_id, ordinal, frame_json)
     VALUES (?, ?, ?)`,
  ).run(frame.sessionId, ordinal, frame.frameJson);
  sqlite.query(
    `UPDATE agent_ui_snapshot_staging
        SET staged_live_bytes = ?, updated_at = ? WHERE session_id = ?`,
  ).run(stagedLiveBytes, now, frame.sessionId);
  return true;
}

function upsertLiveEntry(sqlite: Database, sessionId: string, entry: JsonRecord, now: number): void {
  const validated = requireEntry(entry, "entry.entry");
  const entryJson = JSON.stringify(validated);
  requireReplayableEntryJson(entryJson, "entry.entry");
  const existing = sqlite.query<{ ordinal: number }, [string, string]>(
    "SELECT ordinal FROM agent_ui_entries WHERE session_id = ? AND entry_id = ?",
  ).get(sessionId, validated.id);
  if (existing) {
    sqlite.query(
      `UPDATE agent_ui_entries SET entry_json = ?, updated_at = ?
        WHERE session_id = ? AND entry_id = ?`,
    ).run(entryJson, now, sessionId, validated.id);
    return;
  }
  const ordinal = sqlite.query<{ ordinal: number }, [string]>(
    "SELECT COALESCE(MAX(ordinal), -1) + 1 AS ordinal FROM agent_ui_entries WHERE session_id = ?",
  ).get(sessionId)?.ordinal ?? 0;
  if (ordinal >= SNAPSHOT_MAX_ENTRIES) {
    throw new AgentUiProtocolError(`session exceeds ${SNAPSHOT_MAX_ENTRIES} durable entries`);
  }
  sqlite.query(
    `INSERT INTO agent_ui_entries (session_id, entry_id, ordinal, entry_json, updated_at)
     VALUES (?, ?, ?, ?, ?)`,
  ).run(sessionId, validated.id, ordinal, entryJson, now);
}

function persistLatestFrame(
  sqlite: Database,
  sessionId: string,
  column: "state_json" | "agents_json",
  frameJson: string,
  now: number,
): void {
  sqlite.query(
    `UPDATE agent_ui_sessions SET ${column} = ?, updated_at = ? WHERE session_id = ?`,
  ).run(frameJson, now, sessionId);
}

function applyLiveProjection(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  now: number,
): AgentUiIngestResult {
  if (frame.hostFrame.t === "entry") {
    upsertLiveEntry(sqlite, frame.sessionId, frame.hostFrame.entry as JsonRecord, now);
    return "live-persisted";
  }
  if (frame.hostFrame.t === "state") {
    persistLatestFrame(sqlite, frame.sessionId, "state_json", frame.frameJson, now);
    return "live-persisted";
  }
  if (frame.hostFrame.t === "agents") {
    persistLatestFrame(sqlite, frame.sessionId, "agents_json", frame.frameJson, now);
    return "live-persisted";
  }
  return "relay-only";
}

function persistRevisionedLive(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  revision: number,
  now: number,
): AgentUiIngestResult {
  const result = applyLiveProjection(sqlite, frame, now);
  const completed = sqlite.query<{ snapshot_id: string | null }, [string]>(
    "SELECT snapshot_id FROM agent_ui_sessions WHERE session_id = ?",
  ).get(frame.sessionId)?.snapshot_id;
  if (completed) {
    sqlite.query(
      `INSERT INTO agent_ui_tail_frames (session_id, revision, frame_json) VALUES (?, ?, ?)`,
    ).run(frame.sessionId, revision, frame.frameJson);
  }
  return result;
}

function flushQueuedLive(
  sqlite: Database,
  sessionId: string,
  now: number,
): RevisionedAgentUiRelay[] {
  const rows = sqlite.query<{ frame_json: string }, [string]>(
    `SELECT frame_json FROM agent_ui_live_frame_staging
      WHERE session_id = ? ORDER BY ordinal`,
  ).all(sessionId);
  if (rows.length === 0) return [];

  const relays: RevisionedAgentUiRelay[] = [];
  for (const row of rows) {
    const frame = validateAgentUiFrame({ sessionId, frameJson: row.frame_json, snapshotId: "" });
    const revision = allocateRevision(sqlite, sessionId, now);
    persistRevisionedLive(sqlite, frame, revision, now);
    relays.push({
      session_id: sessionId,
      frame_json: row.frame_json,
      snapshot_id: "",
      coord_revision: revision,
    });
  }
  sqlite.query("DELETE FROM agent_ui_live_frame_staging WHERE session_id = ?").run(sessionId);
  return relays;
}

function abandonSnapshot(
  sqlite: Database,
  sessionId: string,
  now: number,
): RevisionedAgentUiRelay[] {
  const relays = flushQueuedLive(sqlite, sessionId, now);
  clearStaging(sqlite, sessionId);
  return relays;
}

function stageSnapshotChunk(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  now: number,
): AgentUiIngestOutcome {
  const staging = sqlite.query<StagingRow, [string]>(
    `SELECT snapshot_id, baseline_revision, welcome_json, state_json, agents_json,
            expected_entries, staged_bytes, staged_frame_bytes, staged_live_bytes
       FROM agent_ui_snapshot_staging WHERE session_id = ?`,
  ).get(frame.sessionId);
  if (!staging || staging.snapshot_id !== frame.snapshotId) {
    return { result: "snapshot-incomplete", coord_revision: 0, relays: [] };
  }

  const stagedFrameBytes = staging.staged_frame_bytes + byteLength(frame.frameJson);
  if (stagedFrameBytes > SNAPSHOT_WIRE_MAX_BYTES) {
    const relays = abandonSnapshot(sqlite, frame.sessionId, now);
    return {
      result: "snapshot-incomplete",
      coord_revision: relays.at(-1)?.coord_revision ?? currentRevision(sqlite, frame.sessionId),
      relays,
    };
  }
  const frameOrdinal = sqlite.query<{ next_ordinal: number }, [string]>(
    `SELECT COALESCE(MAX(ordinal), -1) + 1 AS next_ordinal
       FROM agent_ui_snapshot_frame_staging WHERE session_id = ?`,
  ).get(frame.sessionId)?.next_ordinal ?? 0;
  sqlite.query(
    `INSERT INTO agent_ui_snapshot_frame_staging (session_id, ordinal, frame_json)
     VALUES (?, ?, ?)`,
  ).run(frame.sessionId, frameOrdinal, frame.frameJson);

  const entries = frame.hostFrame.entries as unknown[];
  let nextOrdinal = sqlite.query<{ next_ordinal: number }, [string]>(
    `SELECT COALESCE(MAX(ordinal), -1) + 1 AS next_ordinal
       FROM agent_ui_snapshot_entries WHERE session_id = ?`,
  ).get(frame.sessionId)?.next_ordinal ?? 0;
  let stagedBytes = staging.staged_bytes;
  const existingStmt = sqlite.query<{ entry_bytes: number }, [string, string]>(
    `SELECT entry_bytes FROM agent_ui_snapshot_entries
      WHERE session_id = ? AND entry_id = ?`,
  );
  const insertStmt = sqlite.query(
    `INSERT INTO agent_ui_snapshot_entries
       (session_id, entry_id, ordinal, entry_json, entry_bytes)
     VALUES (?, ?, ?, ?, ?)`,
  );
  const updateStmt = sqlite.query(
    `UPDATE agent_ui_snapshot_entries SET entry_json = ?, entry_bytes = ?
      WHERE session_id = ? AND entry_id = ?`,
  );
  for (let i = 0; i < entries.length; i++) {
    const entry = requireEntry(entries[i], `snapshot-chunk.entries[${i}]`);
    const entryJson = JSON.stringify(entry);
    requireReplayableEntryJson(entryJson, `snapshot-chunk.entries[${i}]`);
    const entryBytes = byteLength(entryJson);
    const existing = existingStmt.get(frame.sessionId, entry.id);
    if (existing) {
      updateStmt.run(entryJson, entryBytes, frame.sessionId, entry.id);
      stagedBytes += entryBytes - existing.entry_bytes;
    } else {
      if (nextOrdinal >= staging.expected_entries || nextOrdinal >= SNAPSHOT_MAX_ENTRIES) {
        const relays = abandonSnapshot(sqlite, frame.sessionId, now);
        return {
          result: "snapshot-incomplete",
          coord_revision: relays.at(-1)?.coord_revision ?? currentRevision(sqlite, frame.sessionId),
          relays,
        };
      }
      insertStmt.run(frame.sessionId, entry.id, nextOrdinal++, entryJson, entryBytes);
      stagedBytes += entryBytes;
    }
    if (stagedBytes > SNAPSHOT_MAX_BYTES) {
      const relays = abandonSnapshot(sqlite, frame.sessionId, now);
      return {
        result: "snapshot-incomplete",
        coord_revision: relays.at(-1)?.coord_revision ?? currentRevision(sqlite, frame.sessionId),
        relays,
      };
    }
  }
  sqlite.query(
    `UPDATE agent_ui_snapshot_staging
        SET staged_bytes = ?, staged_frame_bytes = ?, updated_at = ?
      WHERE session_id = ?`,
  ).run(stagedBytes, stagedFrameBytes, now, frame.sessionId);
  if (frame.hostFrame.final !== true) {
    return { result: "snapshot-staged", coord_revision: 0, relays: [] };
  }

  const actualEntries = sqlite.query<{ count: number }, [string]>(
    "SELECT COUNT(*) AS count FROM agent_ui_snapshot_entries WHERE session_id = ?",
  ).get(frame.sessionId)?.count ?? 0;
  if (actualEntries !== staging.expected_entries) {
    const relays = abandonSnapshot(sqlite, frame.sessionId, now);
    return {
      result: "snapshot-incomplete",
      coord_revision: relays.at(-1)?.coord_revision ?? currentRevision(sqlite, frame.sessionId),
      relays,
    };
  }
  if (currentRevision(sqlite, frame.sessionId) !== staging.baseline_revision) {
    throw new Error("agent UI staging baseline revision changed before commit");
  }

  sqlite.query("DELETE FROM agent_ui_entries WHERE session_id = ?").run(frame.sessionId);
  sqlite.query(
    `INSERT INTO agent_ui_entries (session_id, entry_id, ordinal, entry_json, updated_at)
     SELECT session_id, entry_id, ordinal, entry_json, ?
       FROM agent_ui_snapshot_entries WHERE session_id = ? ORDER BY ordinal`,
  ).run(now, frame.sessionId);
  sqlite.query("DELETE FROM agent_ui_snapshot_frames WHERE session_id = ?").run(frame.sessionId);
  sqlite.query("DELETE FROM agent_ui_tail_frames WHERE session_id = ?").run(frame.sessionId);
  sqlite.query(
    `UPDATE agent_ui_sessions SET
       snapshot_id = ?, welcome_json = ?, state_json = ?, agents_json = ?, updated_at = ?
     WHERE session_id = ?`,
  ).run(
    frame.snapshotId,
    staging.welcome_json,
    staging.state_json,
    staging.agents_json,
    now,
    frame.sessionId,
  );

  let revision = staging.baseline_revision;
  const snapshotFrameStmt = sqlite.prepare<
    { ordinal: number; frame_json: string },
    [string]
  >(
    `SELECT ordinal, frame_json FROM agent_ui_snapshot_frame_staging
      WHERE session_id = ? ORDER BY ordinal`,
  );
  try {
    for (const snapshotFrame of snapshotFrameStmt.iterate(frame.sessionId)) {
      revision++;
      sqlite.query(
        `INSERT INTO agent_ui_snapshot_frames (session_id, ordinal, revision, frame_json)
         VALUES (?, ?, ?, ?)`,
      ).run(frame.sessionId, snapshotFrame.ordinal, revision, snapshotFrame.frame_json);
    }
  } finally {
    snapshotFrameStmt.finalize();
  }

  const relays: RevisionedAgentUiRelay[] = [];
  const liveFrameStmt = sqlite.prepare<{ frame_json: string }, [string]>(
    `SELECT frame_json FROM agent_ui_live_frame_staging
      WHERE session_id = ? ORDER BY ordinal`,
  );
  try {
    for (const liveFrame of liveFrameStmt.iterate(frame.sessionId)) {
      const validated = validateAgentUiFrame({
        sessionId: frame.sessionId,
        frameJson: liveFrame.frame_json,
        snapshotId: "",
      });
      revision++;
      applyLiveProjection(sqlite, validated, now);
      sqlite.query(
        `INSERT INTO agent_ui_tail_frames (session_id, revision, frame_json) VALUES (?, ?, ?)`,
      ).run(frame.sessionId, revision, liveFrame.frame_json);
      relays.push({
        session_id: frame.sessionId,
        frame_json: liveFrame.frame_json,
        snapshot_id: "",
        coord_revision: revision,
      });
    }
  } finally {
    liveFrameStmt.finalize();
  }
  setRevision(sqlite, frame.sessionId, revision, now);
  clearStaging(sqlite, frame.sessionId);
  return { result: "snapshot-committed", coord_revision: revision, relays };
}

function ingestImmediateLive(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  now: number,
): AgentUiIngestOutcome {
  const revision = allocateRevision(sqlite, frame.sessionId, now);
  const result = persistRevisionedLive(sqlite, frame, revision, now);
  return {
    result,
    coord_revision: revision,
    relays: [{
      session_id: frame.sessionId,
      frame_json: frame.frameJson,
      snapshot_id: "",
      coord_revision: revision,
    }],
  };
}

/** Stage reconciliation frames; revision and persist true-live frames atomically. */
export function ingestAgentUiFrame(
  sqlite: Database,
  frame: ValidatedAgentUiFrame,
  now = Date.now(),
): AgentUiIngestOutcome {
  return sqlite.transaction(() => {
    ensureSessionRow(sqlite, frame.sessionId, now);
    if (frame.snapshotId) {
      if (frame.hostFrame.t === "welcome") {
        const relays = startSnapshot(sqlite, frame, now);
        return {
          result: "snapshot-started" as const,
          coord_revision: relays.at(-1)?.coord_revision ?? 0,
          relays,
        };
      }
      return stageSnapshotChunk(sqlite, frame, now);
    }

    const staging = sqlite.query<{ found: number }, [string]>(
      "SELECT 1 AS found FROM agent_ui_snapshot_staging WHERE session_id = ?",
    ).get(frame.sessionId);
    if (staging) {
      if (queueLiveFrame(sqlite, frame, now)) {
        return { result: "live-staged" as const, coord_revision: 0, relays: [] };
      }
      const queuedRelays = abandonSnapshot(sqlite, frame.sessionId, now);
      const current = ingestImmediateLive(sqlite, frame, now);
      return { ...current, relays: [...queuedRelays, ...current.relays] };
    }

    return ingestImmediateLive(sqlite, frame, now);
  })();
}

function freezeReplayRows(sqlite: Database, sessionId: string): {
  meta: ReplayMetaRow | null;
  rows: ReplayRow[];
} {
  let meta: ReplayMetaRow | null = null;
  let rows: ReplayRow[] = [];
  sqlite.transaction(() => {
    meta = sqlite.query<ReplayMetaRow, [string]>(
      "SELECT snapshot_id FROM agent_ui_sessions WHERE session_id = ?",
    ).get(sessionId) ?? null;
    if (!meta?.snapshot_id) return;
    rows = sqlite.query<ReplayRow, [string, string]>(
      `SELECT revision, frame_json, 1 AS is_snapshot
         FROM agent_ui_snapshot_frames WHERE session_id = ?
       UNION ALL
       SELECT revision, frame_json, 0 AS is_snapshot
         FROM agent_ui_tail_frames WHERE session_id = ?
       ORDER BY revision`,
    ).all(sessionId, sessionId);
  })();
  return { meta, rows };
}

function materializeReplay(sqlite: Database, sessionId: string): MaterializedReplay {
  if (sqlite.filename === ":memory:" || sqlite.filename.length === 0) {
    const frozen = freezeReplayRows(sqlite, sessionId);
    return {
      db: sqlite,
      snapshotId: frozen.meta?.snapshot_id ?? "",
      rows: frozen.rows,
      close: () => {},
    };
  }

  const replay = new Database(sqlite.filename, { readonly: true });
  try {
    replay.exec(
      `PRAGMA temp_store=FILE;
       CREATE TEMP TABLE replay_frames (
         revision INTEGER PRIMARY KEY,
         frame_json TEXT NOT NULL,
         is_snapshot INTEGER NOT NULL
       );
       BEGIN`,
    );
    const meta = replay.query<ReplayMetaRow, [string]>(
      "SELECT snapshot_id FROM agent_ui_sessions WHERE session_id = ?",
    ).get(sessionId) ?? null;
    const snapshotId = meta?.snapshot_id ?? "";
    if (snapshotId) {
      replay.query(
        `INSERT INTO temp.replay_frames (revision, frame_json, is_snapshot)
         SELECT revision, frame_json, 1 FROM agent_ui_snapshot_frames WHERE session_id = ?
         UNION ALL
         SELECT revision, frame_json, 0 FROM agent_ui_tail_frames WHERE session_id = ?`,
      ).run(sessionId, sessionId);
    }
    replay.exec("COMMIT");
    return {
      db: replay,
      snapshotId,
      rows: null,
      close: () => replay.close(),
    };
  } catch (error) {
    try { replay.exec("ROLLBACK"); } catch { /* transaction never opened */ }
    replay.close();
    throw error;
  }
}

function* iterateReplayRows(materialized: MaterializedReplay): Generator<ReplayRow> {
  if (materialized.rows) {
    yield* materialized.rows;
    return;
  }
  const stmt = materialized.db.prepare<ReplayRow, []>(
    "SELECT revision, frame_json, is_snapshot FROM temp.replay_frames ORDER BY revision",
  );
  try {
    yield* stmt.iterate();
  } finally {
    stmt.finalize();
  }
}

/** Stream one atomically materialized snapshot plus its revision-ordered live tail. */
export function* replayAgentUiSnapshot(
  sqlite: Database,
  sessionId: string,
): Generator<StoredAgentUiFrame> {
  const materialized = materializeReplay(sqlite, sessionId);
  try {
    if (!materialized.snapshotId) return;
    let sawWelcome = false;
    let sawFinalChunk = false;
    let previousRevision = 0;
    for (const row of iterateReplayRows(materialized)) {
      if (!Number.isSafeInteger(row.revision) || row.revision <= previousRevision) {
        throw new Error("stored agent UI revisions are not strictly increasing");
      }
      const snapshotId = row.is_snapshot ? materialized.snapshotId : "";
      const validated = validateAgentUiFrame({
        sessionId,
        frameJson: row.frame_json,
        snapshotId,
      });
      if (row.is_snapshot) {
        if (!sawWelcome) {
          if (validated.hostFrame.t !== "welcome") throw new Error("stored snapshot does not start with welcome");
          sawWelcome = true;
        } else if (validated.hostFrame.t !== "snapshot-chunk") {
          throw new Error("stored snapshot contains a non-chunk frame after welcome");
        } else if (validated.hostFrame.final === true) {
          sawFinalChunk = true;
        }
      } else if (!sawFinalChunk) {
        throw new Error("stored snapshot has no final chunk before its live tail");
      }
      previousRevision = row.revision;
      yield {
        frame_json: row.frame_json,
        snapshot_id: snapshotId,
        coord_revision: row.revision,
      };
    }
    if (!sawWelcome || !sawFinalChunk) throw new Error("stored snapshot train is incomplete");
  } finally {
    materialized.close();
  }
}

export interface AgentUiStagingSweepOutcome {
  readonly deleted: number;
  readonly relays: readonly RevisionedAgentUiRelay[];
}

export function sweepAgentUiSnapshotStaging(
  sqlite: Database,
  now = Date.now(),
): AgentUiStagingSweepOutcome {
  const cutoff = now - STAGING_TTL_MS;
  return sqlite.transaction(() => {
    const stale = sqlite.query<{ session_id: string }, [number]>(
      "SELECT session_id FROM agent_ui_snapshot_staging WHERE updated_at < ?",
    ).all(cutoff);
    const relays: RevisionedAgentUiRelay[] = [];
    for (const row of stale) {
      relays.push(...abandonSnapshot(sqlite, row.session_id, now));
    }
    return { deleted: stale.length, relays };
  })();
}

export type AgentUiRelayPublisher = (relay: RevisionedAgentUiRelay) => void;

function runStagingSweep(sqlite: Database, publish: AgentUiRelayPublisher): void {
  try {
    const outcome = sweepAgentUiSnapshotStaging(sqlite);
    for (const relay of outcome.relays) publish(relay);
    if (outcome.deleted > 0) {
      log.info("agent-ui", "snapshot_staging_pruned", {
        deleted: outcome.deleted,
        relayed: outcome.relays.length,
      });
    }
  } catch (error) {
    log.warn("agent-ui", "snapshot_staging_prune_failed", { error: String(error) });
  }
}

export function scheduleAgentUiSnapshotStagingCleanup(
  sqlite: Database,
  publish: AgentUiRelayPublisher,
): void {
  runStagingSweep(sqlite, publish);
  setInterval(
    () => runStagingSweep(sqlite, publish),
    STAGING_SWEEP_INTERVAL_MS,
  ).unref();
}

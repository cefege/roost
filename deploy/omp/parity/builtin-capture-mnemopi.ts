import { Database } from "bun:sqlite";
import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { ownedPath } from "./builtin-capture-state";

interface DatabaseObservation {
  rowCount: number;
  lessonPresent: boolean;
  retainedTurns?: number | null;
}

function observeDatabase(database: Database): DatabaseObservation {
  let rowCount = 0;
  let lessonPresent = false;
  const retained: Array<number | null> = [];
  for (const table of ["working_memory", "episodic_memory"]) {
    using columnsQuery = database.prepare<{ name: string }, []>(`PRAGMA table_info(${table})`);
    const columns = new Set(columnsQuery.all().map(column => column.name));
    if (!columns.has("content")) throw new Error(`Actual Mnemopi database lacks ${table}.content`);
    using rowsQuery = database.prepare<{ rowCount: number; lessonPresent: number }, [string]>(`SELECT COUNT(*) AS rowCount, COUNT(CASE WHEN instr(content, ?) > 0 THEN 1 END) AS lessonPresent FROM ${table}`);
    const rows = rowsQuery.get("PARITY_SQLITE_LESSON");
    if (!rows) throw new Error(`Actual Mnemopi database returned no ${table} aggregate`);
    rowCount += rows.rowCount;
    lessonPresent ||= rows.lessonPresent > 0;
    if (columns.has("metadata_json") && columns.has("source")) {
      using retainedQuery = database.prepare<{ retainedTurns: number | null }, []>(`SELECT MAX(CAST(json_extract(metadata_json, '$.retained_through_user_turn') AS INTEGER)) AS retainedTurns FROM ${table} WHERE source = 'coding-agent-transcript'`);
      const cursor = retainedQuery.get();
      if (!cursor) throw new Error(`Actual Mnemopi database returned no ${table} retention aggregate`);
      retained.push(cursor.retainedTurns);
    }
  }
  const cursors = retained.filter((cursor): cursor is number => cursor !== null);
  return { rowCount, lessonPresent, ...(retained.length > 0 ? { retainedTurns: cursors.length > 0 ? Math.max(...cursors) : null } : {}) };
}

export async function captureMnemopi(state: CaptureState, session: AgentSession): Promise<unknown> {
  const memory = session.getMnemopiSessionState();
  if (!memory) throw new Error("Required actual Mnemopi session state is absent");
  const primary = memory.aliasOf ?? memory;
  const states = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/mnemopi/state")>("packages/coding-agent/src/mnemopi/state.ts");
  const paths = [...new Set(states.getMnemopiScopedDbPaths(primary.config))];
  if (paths.length === 0) throw new Error("Actual Mnemopi state has no scoped databases");
  for (const path of paths) await ownedPath(state, path);
  const liveMemories = new Set([primary.memory, ...primary.getScopedRecallTargets().map(target => target.memory), ...(primary.globalMemory ? [primary.globalMemory] : [])]);
  const live = [...liveMemories].map(memory => observeDatabase(memory.beam.db));
  const disk: DatabaseObservation[] = [];
  const integrity: string[] = [];
  for (const path of paths) {
    const database = new Database(await ownedPath(state, path), { readonly: true });
    try {
      disk.push(observeDatabase(database));
      using integrityQuery = database.prepare<{ integrity_check: string }, []>("PRAGMA integrity_check");
      const rows = integrityQuery.all();
      if (rows.length === 0) throw new Error("Actual Mnemopi integrity_check returned no result");
      integrity.push(...rows.map(row => row.integrity_check));
    } finally { database.close(); }
  }
  const cursors = disk.flatMap(observation => typeof observation.retainedTurns === "number" ? [observation.retainedTurns] : []);
  return {
    lessonPresent: live.some(observation => observation.lessonPresent),
    diskLessonPresent: disk.some(observation => observation.lessonPresent),
    rowCount: live.reduce((sum, observation) => sum + observation.rowCount, 0),
    diskRowCount: disk.reduce((sum, observation) => sum + observation.rowCount, 0),
    retainedTurns: primary.lastRetainedTurn,
    ...(disk.some(observation => observation.retainedTurns !== undefined) ? { diskRetainedTurns: cursors.length > 0 ? Math.max(...cursors) : null } : {}),
    diskIntegrity: [...new Set(integrity)].join("\n"),
  };
}

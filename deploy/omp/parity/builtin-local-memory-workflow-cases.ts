import type { BuiltinCase } from "./scenario";
import { commandCase, effect, escape, observed, send } from "./builtin-steps";

const sqliteSettings = {
  "memory.backend": "mnemopi", "mnemopi.bank": "parity-sqlite", "mnemopi.scoping": "global",
  "mnemopi.noEmbeddings": true, "mnemopi.llmMode": "none", "mnemopi.autoRecall": false,
  "mnemopi.autoRetain": false, "mnemopi.proactiveLinking": false, "autolearn.enabled": true,
};
const stored = "PARITY_SQLITE_STORED";
const closeSqliteReport = () => ({ ...observed(stored), terminalInput: "\u001b", browserActions: escape });
const queueRule = "Corrective rule: PARITY_DECISION is mandatory because silent fallback regresses owned settings.";

export const localMemoryWorkflowCases: BuiltinCase[] = [
  {
    ...commandCase("memory-sqlite-durable-lifecycle", "/force learn Store PARITY_SQLITE_LESSON as a durable local fact.", stored, [
      send("/memory stats", "Mnemopi Memory Stats", [effect("builtin.memory", true, "/mnemopi/diskLessonPresent")]),
      closeSqliteReport(),
      send("/memory diagnose", "Mnemopi Memory Diagnostics", [effect("builtin.memory", "ok", "/mnemopi/diskIntegrity")]),
      closeSqliteReport(),
      send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount"), effect("builtin.memory", true, "/mnemopi/diskLessonPresent")]),
      send("/memory view", "Memory Injection Payload", [effect("builtin.memory", true, "/mnemopi/lessonPresent")]),
      closeSqliteReport(),
      send("/memory enqueue", "Memory consolidation enqueued.", [effect("builtin.memory", 1, "/mnemopi/retainedTurns"), effect("builtin.memory", 1, "/mnemopi/diskRetainedTurns")]),
      send("Parity second local memory turn.", "PARITY_SQLITE_SECOND"),
      send("/memory sync", "Memory consolidation ran.", [effect("builtin.memory", 2, "/mnemopi/retainedTurns"), effect("builtin.memory", 2, "/mnemopi/diskRetainedTurns")]),
      send("Parity third local memory turn.", "PARITY_SQLITE_THIRD"),
      send("/memory rebuild", "Memory consolidation enqueued.", [effect("builtin.memory", 3, "/mnemopi/retainedTurns"), effect("builtin.memory", 3, "/mnemopi/diskRetainedTurns")]),
      send("/memory clear", "Memory data cleared and system prompt refreshed.", [effect("builtin.memory", 0, "/mnemopi/rowCount"), effect("builtin.memory", 0, "/mnemopi/diskRowCount"), effect("builtin.memory", false, "/mnemopi/diskLessonPresent")]),
    ], [effect("builtin.memory", true, "/mnemopi/lessonPresent"), effect("builtin.memory", true, "/mnemopi/diskLessonPresent")]),
    settings: sqliteSettings,
  },
  {
    ...commandCase("memory-sqlite-reset", "/force learn Store PARITY_SQLITE_LESSON before reset.", stored, [
      send("/memory reset", "Memory data cleared and system prompt refreshed.", [effect("builtin.memory", 0, "/mnemopi/rowCount"), effect("builtin.memory", 0, "/mnemopi/diskRowCount")]),
      send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount"), effect("builtin.memory", false, "/mnemopi/diskLessonPresent"), effect("builtin.memory", "ok", "/mnemopi/diskIntegrity")]),
    ], [effect("builtin.memory", true, "/mnemopi/diskLessonPresent")]),
    settings: sqliteSettings,
  },
  {
    ...commandCase("memory-sharpshooter-extracted-queue", `/queue ${queueRule}`, "PARITY_SHARPSHOOTER_READY", [
      send("/memory queue", "Pending Sharpshooter Deltas", [effect("builtin.memory", 1, "/sharpshooter/queueDepth"), effect("builtin.memory", ["PARITY_DECISION is mandatory."], "/sharpshooter/statements")]),
      { ...observed("PARITY_SHARPSHOOTER_READY"), terminalInput: "\u001b", browserActions: escape },
      send("/memory sync", "Memory consolidation ran.", [effect("builtin.memory", 0, "/sharpshooter/queueDepth"), effect("builtin.memory", "PARITY_DECISION is mandatory.\n", "/sharpshooter/files/architecture.md"), effect("builtin.memory", 1, "/sharpshooter/state/lastResult/deltas")]),
      send("/memory queue", "Queue is empty.", [effect("builtin.memory", 0, "/sharpshooter/queueDepth")]),
      { ...observed("PARITY_SHARPSHOOTER_READY"), terminalInput: "\u001b", browserActions: escape },
      send("/memory view", "Memory Injection Payload", [effect("builtin.memory", "PARITY_DECISION is mandatory.\n", "/sharpshooter/files/architecture.md")]),
      { ...observed("PARITY_SHARPSHOOTER_READY"), terminalInput: "\u001b", browserActions: escape },
      send("/restart", "Parity session", [effect("builtin.process", 1, "/restartCount"), effect("builtin.memory", "PARITY_DECISION is mandatory.\n", "/sharpshooter/files/architecture.md")]),
      send("/memory clear", "Memory data cleared and system prompt refreshed.", [effect("builtin.memory", false, "/sharpshooter/bankExists"), effect("builtin.memory", 0, "/sharpshooter/queueDepth")]),
    ], [effect("builtin.memory", 1, "/sharpshooter/queueDepth")]),
    settings: { "memory.backend": "sharpshooter", "sharpshooter.model": "openai/parity-small", "sharpshooter.intervalMinutes": 1_000_000_000 },
  },
];

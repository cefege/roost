// Pinned Todos HUD projection: decoding the newest successful `todo` envelope
// off the wire, then omp's collapsed-view budgets (active phase + 4 followers,
// 5 tasks previewed) on top of it. Pure functions, no store/RPC — matches the
// chatWelcome test style (bun:test, direct calls).
//
// The decode cases run against apps/web/src/chat-render-real.json, a real omp
// transcript, so the envelope shape is the one the worker actually emits rather
// than a hand-written guess.

import { expect, test, describe } from "bun:test";
import type { ChatMessage } from "@roost/shared/chat/wire";
import fixture from "../src/chat-render-real.json";
import {
  latestTodoPhases, buildTodoHud, phaseRomanNumeral,
  type TodoItem, type TodoPhase, type TodoStatus,
} from "../src/components/chat/omp/todoHud.ts";

const REAL: ChatMessage[] = fixture as ChatMessage[];

/** A one-message thread carrying a single todo result, envelope supplied. */
function threadWith(rawJson: string, isError = false): ChatMessage[] {
  return [{
    id: "m1", parentId: "", ts: "0", role: "toolResult", synthetic: false,
    blocks: [{ kind: "toolResult", callId: "c1", name: "todo", text: "", isError, truncated: false, fullLen: 0, rawJson }],
  }];
}

function envelope(phases: unknown): string {
  return JSON.stringify({ toolCallId: "c1", toolName: "todo", isError: false, content: [], details: { op: "init", phases, storage: "session" } });
}

function tasks(...specs: Array<[string, TodoStatus]>): TodoItem[] {
  return specs.map(([content, status]) => ({ content, status }));
}

describe("latestTodoPhases", () => {
  test("decodes the board out of a real omp todo envelope", () => {
    const phases = latestTodoPhases(REAL);
    expect(phases.length).toBe(5);
    expect(phases[0]!.name).toBe("Wire contract");
    expect(phases[0]!.tasks.map((t) => t.content)).toEqual([
      "Add proto messages + RPCs", "Regenerate proto code", "Add control frames",
    ]);
    expect(phases[0]!.tasks[0]!.status).toBe("in_progress");
    expect(phases[0]!.tasks[1]!.status).toBe("pending");
  });

  test("returns [] with no thread, no todo result, or unparseable JSON", () => {
    expect(latestTodoPhases([])).toEqual([]);
    expect(latestTodoPhases(REAL.filter((m) => !m.blocks.some((b) => b.kind === "toolResult" && b.name === "todo")))).toEqual([]);
    expect(latestTodoPhases(threadWith("{"))).toEqual([]);
    expect(latestTodoPhases(threadWith(""))).toEqual([]);
    expect(latestTodoPhases(threadWith(JSON.stringify({ details: { op: "view" } })))).toEqual([]);
  });

  test("skips an error result and keeps the last good board", () => {
    const good = envelope([{ name: "Kept", tasks: [{ content: "a", status: "pending" }] }]);
    const bad = envelope([{ name: "Failed", tasks: [{ content: "b", status: "pending" }] }]);
    const thread = [...threadWith(good), ...threadWith(bad, true)];
    expect(latestTodoPhases(thread).map((p) => p.name)).toEqual(["Kept"]);
  });

  test("takes the newest board when several succeeded", () => {
    const thread = [
      ...threadWith(envelope([{ name: "Old", tasks: [{ content: "a", status: "pending" }] }])),
      ...threadWith(envelope([{ name: "New", tasks: [{ content: "b", status: "pending" }] }])),
    ];
    expect(latestTodoPhases(thread).map((p) => p.name)).toEqual(["New"]);
  });

  test("keeps blocked + its note, coerces an unknown status to pending", () => {
    const phases = latestTodoPhases(threadWith(envelope([{
      name: "P",
      tasks: [
        { content: "waiting", status: "blocked", blocker: "needs key" },
        { content: "bare block", status: "blocked" },
        { content: "weird", status: "sideways" },
        { content: "missing" },
      ],
    }])));
    expect(phases[0]!.tasks).toEqual([
      { content: "waiting", status: "blocked", blocker: "needs key" },
      { content: "bare block", status: "blocked" },
      { content: "weird", status: "pending" },
      { content: "missing", status: "pending" },
    ]);
  });

  test("drops malformed phases and tasks instead of rendering them half-built", () => {
    const phases = latestTodoPhases(threadWith(envelope([
      null,
      { name: 7, tasks: [] },
      { name: "no tasks array", tasks: "nope" },
      { name: "Good", tasks: [{ content: "keep", status: "pending" }, { status: "pending" }, null, "x"] },
    ])));
    expect(phases).toEqual([{ name: "Good", tasks: [{ content: "keep", status: "pending" }] }]);
  });

  test("returns [] when details.phases is not an array", () => {
    expect(latestTodoPhases(threadWith(envelope("nope")))).toEqual([]);
    expect(latestTodoPhases(threadWith(JSON.stringify({ toolName: "todo" })))).toEqual([]);
  });
});

describe("buildTodoHud", () => {
  test("returns null when there is no board to paint", () => {
    expect(buildTodoHud([], false)).toBeNull();
    expect(buildTodoHud([{ name: "x", tasks: [] }], false)).toBeNull();
  });

  test("collapsed: active phase plus four followers, tasks only on the active one", () => {
    const phases: TodoPhase[] = Array.from({ length: 8 }, (_, i) => ({
      name: `P${i + 1}`,
      tasks: tasks([`t${i}a`, "pending"], [`t${i}b`, "pending"]),
    }));
    const hud = buildTodoHud(phases, false)!;
    expect(hud.activeIndex).toBe(1);
    expect(hud.phaseCount).toBe(8);
    expect(hud.phases.length).toBe(5);
    expect(hud.phases.map((p) => p.index)).toEqual([1, 2, 3, 4, 5]);
    expect(hud.phases[0]!.active).toBe(true);
    expect(hud.phases[0]!.tasks.length).toBe(2);
    expect(hud.phases.slice(1).every((p) => !p.active && p.tasks.length === 0 && p.summary === "")).toBe(true);
  });

  test("empty phases drop out but the surviving ones keep their real numbering", () => {
    const hud = buildTodoHud([
      { name: "Done", tasks: tasks(["a", "completed"]) },
      { name: "Empty", tasks: [] },
      { name: "Now", tasks: tasks(["b", "in_progress"]) },
    ], false)!;
    expect(hud.phaseCount).toBe(2);
    expect(hud.activeIndex).toBe(2);
    expect(hud.phases.map((p) => p.index)).toEqual([2]);
    expect(hud.phases[0]!.name).toBe("Now");
  });

  test("counts done/total per phase", () => {
    const hud = buildTodoHud([{ name: "P", tasks: tasks(["a", "completed"], ["b", "completed"], ["c", "pending"]) }], false)!;
    expect(hud.phases[0]!.done).toBe(2);
    expect(hud.phases[0]!.total).toBe(3);
  });

  test("expanded: every phase, every task, no summaries", () => {
    const phases: TodoPhase[] = Array.from({ length: 8 }, (_, i) => ({
      name: `P${i + 1}`,
      tasks: tasks([`t${i}a`, "pending"], [`t${i}b`, "pending"]),
    }));
    const hud = buildTodoHud(phases, true)!;
    expect(hud.phases.length).toBe(8);
    expect(hud.phases.map((p) => p.index)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
    expect(hud.phases.every((p) => p.tasks.length === 2 && p.summary === "")).toBe(true);
  });

  test("expanded keeps the roman numerals tied to the real index when phase 3 is active", () => {
    const phases: TodoPhase[] = [
      { name: "A", tasks: tasks(["a", "completed"]) },
      { name: "B", tasks: tasks(["b", "completed"]) },
      { name: "C", tasks: tasks(["c", "in_progress"]) },
    ];
    expect(buildTodoHud(phases, false)!.phases.map((p) => p.index)).toEqual([3]);
    expect(buildTodoHud(phases, true)!.phases.map((p) => p.index)).toEqual([1, 2, 3]);
    expect(buildTodoHud(phases, true)!.activeIndex).toBe(3);
  });

  test("collapsed active phase: in_progress leads, followers fill to 5, rest summarized", () => {
    const board: TodoPhase[] = [{
      name: "Big",
      tasks: [
        ...tasks(["p1", "pending"], ["p2", "pending"]),
        { content: "now", status: "in_progress" },
        ...tasks(["p3", "pending"], ["p4", "pending"], ["p5", "pending"], ["p6", "pending"], ["p7", "pending"], ["p8", "pending"]),
      ],
    }];
    const phase = buildTodoHud(board, false)!.phases[0]!;
    expect(phase.tasks.map((t) => t.content)).toEqual(["now", "p3", "p4", "p5", "p6"]);
    expect(phase.summary).toBe("… 4 more todos");
  });

  test("one hidden todo is singular", () => {
    const board: TodoPhase[] = [{
      name: "Six",
      tasks: [{ content: "now", status: "in_progress" }, ...tasks(["a", "pending"], ["b", "pending"], ["c", "pending"], ["d", "pending"], ["e", "pending"])],
    }];
    const phase = buildTodoHud(board, false)!.phases[0]!;
    expect(phase.tasks.length).toBe(5);
    expect(phase.summary).toBe("… 1 more todo");
  });

  test("more in_progress than the cap counts hidden actives, never pending rows", () => {
    const board: TodoPhase[] = [{
      name: "Swarm",
      tasks: [...Array.from({ length: 7 }, (_, i) => ({ content: `a${i}`, status: "in_progress" as TodoStatus })), ...tasks(["later", "pending"])],
    }];
    const phase = buildTodoHud(board, false)!.phases[0]!;
    expect(phase.tasks.map((t) => t.content)).toEqual(["a0", "a1", "a2", "a3", "a4"]);
    expect(phase.summary).toBe("… 2 more active todos");
  });

  test("closed tasks are omitted while the phase still has open work", () => {
    const board: TodoPhase[] = [{
      name: "Mixed",
      tasks: [...tasks(["c1", "completed"], ["c2", "completed"], ["c3", "abandoned"]), ...tasks(["o1", "pending"], ["o2", "pending"], ["o3", "pending"], ["o4", "pending"], ["o5", "pending"], ["o6", "pending"])],
    }];
    const phase = buildTodoHud(board, false)!.phases[0]!;
    expect(phase.tasks.map((t) => t.content)).toEqual(["o1", "o2", "o3", "o4", "o5"]);
    expect(phase.summary).toBe("… 1 more todo");
    expect(phase.done).toBe(2);
    expect(phase.total).toBe(9);
  });

  test("a fully settled board keeps its LAST phase active and still renders its closed tasks", () => {
    const board: TodoPhase[] = [
      { name: "First", tasks: tasks(["a", "completed"]) },
      { name: "Last", tasks: tasks(["b", "completed"], ["c", "abandoned"]) },
    ];
    const hud = buildTodoHud(board, false)!;
    expect(hud.activeIndex).toBe(2);
    expect(hud.phases.map((p) => p.index)).toEqual([2]);
    expect(hud.phases[0]!.active).toBe(true);
    expect(hud.phases[0]!.tasks.map((t) => t.content)).toEqual(["b", "c"]);
    expect(hud.phases[0]!.summary).toBe("");
  });

  test("blocked alone never makes a phase active — it is waiting, not open work", () => {
    const board: TodoPhase[] = [
      { name: "Stuck", tasks: [{ content: "waiting", status: "blocked", blocker: "needs key" }] },
      { name: "Open", tasks: tasks(["go", "pending"]) },
    ];
    expect(buildTodoHud(board, false)!.activeIndex).toBe(2);
    expect(buildTodoHud(board, false)!.phases[0]!.name).toBe("Open");
    // Expanded still shows the stalled phase and keeps its blocker note.
    const stuck = buildTodoHud(board, true)!.phases[0]!;
    expect(stuck.name).toBe("Stuck");
    expect(stuck.tasks[0]!.blocker).toBe("needs key");
  });

  test("the real fixture board collapses to its first phase", () => {
    const hud = buildTodoHud(latestTodoPhases(REAL), false)!;
    expect(hud.activeIndex).toBe(1);
    expect(hud.phaseCount).toBe(5);
    expect(hud.phases.length).toBe(5);
    expect(hud.phases[0]!.tasks[0]!.status).toBe("in_progress");
  });
});

describe("phaseRomanNumeral", () => {
  test("renders the header numerals", () => {
    expect([1, 4, 5, 9, 14].map(phaseRomanNumeral)).toEqual(["I", "IV", "V", "IX", "XIV"]);
  });

  test("is empty at or below zero", () => {
    expect(phaseRomanNumeral(0)).toBe("");
    expect(phaseRomanNumeral(-3)).toBe("");
  });
});

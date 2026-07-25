// Projection for the pinned Todos HUD above the composer — the web port of
// omp's anchored todoContainer (modes/interactive-mode.ts #renderTodoList).
// That HUD is TUI chrome, not a transcript entry, so nothing on the wire
// carries it; the board is re-derived here from the newest successful `todo`
// toolResult already in the thread. Pure (no Solid, no store) so the selection
// policy is directly testable, same split as welcomeTips.ts / renderPlan.ts.

import type { ChatMessage } from "@roost/shared/chat/wire";
import { safeJsonParse } from "@roost/shared/json";

export type TodoStatus = "pending" | "in_progress" | "completed" | "abandoned" | "blocked";
export interface TodoItem { content: string; status: TodoStatus; blocker?: string }
export interface TodoPhase { name: string; tasks: TodoItem[] }

/** Rows the HUD paints for one phase, plus omp's trailing "… N more todos". */
export interface PhaseView {
  /** 1-based index into the FULL phase list — roman numerals stay tied to it. */
  index: number;
  name: string;
  done: number;
  total: number;
  active: boolean;
  /** Empty when the phase is collapsed-and-inactive: header only. */
  tasks: TodoItem[];
  /** "" when nothing is hidden. */
  summary: string;
}

export interface TodoHudView {
  phases: PhaseView[];
  /** 1-based index of the active phase in the full list. */
  activeIndex: number;
  /** Count of phases with at least one task — the "/N" in the header. */
  phaseCount: number;
}

/** omp's fixed HUD budgets (interactive-mode.ts:2049-2050): stages shown after
 *  the active one, and open tasks previewed for the active stage. Fixed so the
 *  panel stays bounded regardless of plan size. */
const SUBSEQUENT_PHASE_CAP = 4;
const ACTIVE_TASK_CAP = 5;

const STATUSES: readonly TodoStatus[] = ["pending", "in_progress", "completed", "abandoned", "blocked"];

/** The newest todo board on the wire, or [] when the thread holds none.
 *
 *  Walks backwards to the first non-error `todo` toolResult and decodes
 *  `details.phases` out of its omp envelope. An error-flagged result is skipped
 *  rather than clearing the board, matching omp's getLatestTodoPhasesFromEntries
 *  — a failed `todo done` must not blank the HUD. */
export function latestTodoPhases(messages: readonly ChatMessage[]): TodoPhase[] {
  for (let i = messages.length - 1; i >= 0; i--) {
    const blocks = messages[i]?.blocks ?? [];
    for (let j = blocks.length - 1; j >= 0; j--) {
      const b = blocks[j];
      if (!b || b.kind !== "toolResult" || b.name !== "todo" || b.isError || b.rawJson === "") continue;
      return decodePhases(safeJsonParse<unknown>(b.rawJson, null, "chat.todoHud.rawJson"));
    }
  }
  return [];
}

/** `details.phases` out of an omp ToolResultMessage envelope. Guarded field by
 *  field like pickTips: it crossed a JSON tunnel, so a malformed phase or task
 *  is dropped rather than rendered half-built. */
function decodePhases(envelope: unknown): TodoPhase[] {
  if (!envelope || typeof envelope !== "object" || !("details" in envelope)) return [];
  const details = envelope.details;
  if (!details || typeof details !== "object" || !("phases" in details)) return [];
  const raw = details.phases;
  if (!Array.isArray(raw)) return [];
  const out: TodoPhase[] = [];
  for (const p of raw) {
    if (!p || typeof p !== "object") continue;
    const name = "name" in p && typeof p.name === "string" ? p.name : null;
    const rawTasks = "tasks" in p ? p.tasks : null;
    if (name === null || !Array.isArray(rawTasks)) continue;
    const tasks: TodoItem[] = [];
    for (const t of rawTasks) {
      if (!t || typeof t !== "object") continue;
      if (!("content" in t) || typeof t.content !== "string") continue;
      const rawStatus = "status" in t ? t.status : null;
      const status = STATUSES.find((s) => s === rawStatus) ?? "pending";
      const blocker = "blocker" in t && typeof t.blocker === "string" ? t.blocker : undefined;
      tasks.push(blocker === undefined ? { content: t.content, status } : { content: t.content, status, blocker });
    }
    out.push({ name, tasks });
  }
  return out;
}

/** The rows the panel paints, or null when there is no board to show.
 *
 *  Port of #renderTodoList (interactive-mode.ts:2041): empty phases drop out,
 *  the first phase with open work is active (else the last one), and collapsed
 *  shows that phase plus SUBSEQUENT_PHASE_CAP followers — the header's "n/N"
 *  implies the rest. Expanded lists every phase and every task. */
export function buildTodoHud(phases: readonly TodoPhase[], expanded: boolean): TodoHudView | null {
  const visible = phases.filter((p) => p.tasks.length > 0);
  if (visible.length === 0) return null;

  // #getActivePhase (interactive-mode.ts:2001): first phase with open work, and
  // when the whole plan is settled the LAST one — a finished board keeps its
  // final phase on screen rather than snapping back to the top.
  const found = visible.findIndex((p) => p.tasks.some((t) => t.status === "pending" || t.status === "in_progress"));
  const activeIdx = found === -1 ? visible.length - 1 : found;
  const slice = expanded ? visible : visible.slice(activeIdx, activeIdx + 1 + SUBSEQUENT_PHASE_CAP);
  const base = expanded ? 0 : activeIdx;

  const out: PhaseView[] = slice.map((p, i) => {
    const active = base + i === activeIdx;
    const selected = expanded
      ? { items: p.tasks, summary: "" }
      : active
        ? selectCollapsed(p.tasks, ACTIVE_TASK_CAP)
        : { items: [] as TodoItem[], summary: "" };
    return {
      index: base + i + 1,
      name: p.name,
      done: p.tasks.filter((t) => t.status === "completed").length,
      total: p.tasks.length,
      active,
      tasks: selected.items,
      summary: selected.summary,
    };
  });

  return { phases: out, activeIndex: activeIdx + 1, phaseCount: visible.length };
}

/** Port of omp's selectCollapsedTodos (tools/todo.ts:276) with the
 *  subagent-match predicate pinned to false — the browser has no
 *  active-subagent-description feed, so isActiveTodo reduces to `in_progress`.
 *
 *  While a phase has open work its closed tasks are omitted; a settled phase
 *  falls back to its closed tasks so it still renders. In-progress tasks lead,
 *  the rows after the first of them fill the remaining budget. */
function selectCollapsed(tasks: readonly TodoItem[], cap: number): { items: TodoItem[]; summary: string } {
  const open = tasks.filter((t) => t.status === "pending" || t.status === "in_progress" || t.status === "blocked");
  const base = open.length > 0 ? open : tasks;
  if (base.length <= cap) return { items: [...base], summary: "" };

  const active = base.filter((t) => t.status === "in_progress");
  // Only when active work strictly exceeds the cap do we drop pending rows and
  // count hidden *actives*. At exactly `cap`, fall through so the normal branch
  // still surfaces the following pending work in the summary.
  if (active.length > cap) {
    const hidden = active.length - cap;
    // omp's formatMoreItems(hidden, "todo") tail (tools/render-utils.ts:201).
    return { items: active.slice(0, cap), summary: `… ${hidden} more active ${hidden === 1 ? "todo" : "todos"}` };
  }

  const firstActiveIdx = active.length > 0 ? base.indexOf(active[0]!) : 0;
  const fill: TodoItem[] = [];
  for (let i = firstActiveIdx; i < base.length && active.length + fill.length < cap; i++) {
    const task = base[i]!;
    if (task.status === "in_progress") continue;
    fill.push(task);
  }
  const items = [...active, ...fill];
  const hidden = base.length - items.length;
  return { items, summary: hidden > 0 ? `… ${hidden} more ${hidden === 1 ? "todo" : "todos"}` : "" };
}

const ROMAN_PAIRS: ReadonlyArray<readonly [number, string]> = [
  [1000, "M"], [900, "CM"], [500, "D"], [400, "CD"], [100, "C"], [90, "XC"],
  [50, "L"], [40, "XL"], [10, "X"], [9, "IX"], [5, "V"], [4, "IV"], [1, "I"],
];

/** One-based ASCII roman numeral for a phase header (I, II, III, IV, …).
 *  Verbatim port of tools/todo.ts:919. */
export function phaseRomanNumeral(oneBasedIndex: number): string {
  if (oneBasedIndex <= 0) return "";
  let out = "";
  let rem = oneBasedIndex;
  for (const [value, sym] of ROMAN_PAIRS) {
    while (rem >= value) {
      out += sym;
      rem -= value;
    }
  }
  return out;
}

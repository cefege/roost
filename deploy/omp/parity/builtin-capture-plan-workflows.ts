import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { CaptureState } from "./builtin-capture-state";
import { boundaryValue, objectValue, requestedPointers } from "./builtin-capture-state";
import { readPersistedEntries } from "./builtin-capture-session-workflows";

function advisorNotes(details: unknown): unknown[] {
  const record = objectValue(details, "Actual advisor message details");
  if (!Array.isArray(record.notes)) throw new Error("Actual advisor message has no notes array");
  return record.notes;
}

export async function capturePlanWorkflows(state: CaptureState, session: AgentSession): Promise<Record<string, unknown>> {
  const pointers = requestedPointers(state, "builtin.session");
  const observation: Record<string, unknown> = {};
  if (pointers.some(parts => parts[0] === "planReferencePath")) observation.planReferencePath = session.getPlanReferencePath();
  if (pointers.some(parts => parts[0] === "planModeState")) observation.planModeState = session.getPlanModeState() ?? null;
  if (pointers.some(parts => parts[0] === "persistedMode" || parts[0] === "persistedGoal")) {
    const context = session.sessionManager.buildSessionContext();
    const goals = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/goals/state")>("packages/coding-agent/src/goals/state.ts");
    observation.persistedMode = context.mode;
    observation.persistedGoal = goals.goalFromModeData(context.modeData) ?? null;
  }
  if (pointers.some(parts => parts[0] === "completedGoal")) {
    const completed = session.sessionManager.getEntries().findLast(entry => entry.type === "custom" && entry.customType === "goal-completed");
    if (!completed || completed.type !== "custom") throw new Error("No actual goal-completed session entry observed");
    observation.completedGoal = completed.data;
  }
  const advisorPointers = pointers.filter(parts => parts[0] === "advisor");
  if (advisorPointers.length > 0) {
    const advisor: Record<string, unknown> = { stats: session.getAdvisorStats(), transcript: session.formatAdvisorHistoryAsText({ compact: false }) };
    if (advisorPointers.some(parts => parts[1] === "notes")) {
      advisor.notes = session.messages.flatMap(message => message.role === "custom" && message.customType === "advisor" ? advisorNotes(message.details) : []);
    }
    if (advisorPointers.some(parts => parts[1] === "persistedNotes")) {
      const file = session.sessionManager.getSessionFile();
      if (!file) throw new Error("Persisted advisor capture requires the actual session file");
      const entries = await readPersistedEntries(state, file);
      advisor.persistedNotes = entries.flatMap(entry => entry.type === "custom_message" && entry.customType === "advisor" ? advisorNotes(entry.details) : []);
    }
    if (advisorPointers.some(parts => parts[1] === "clipboardMatchesHistory")) {
      const clipboard = await boundaryValue(state, "OMP_PARITY_HOST_TRACE", "builtin.clipboard");
      if (typeof clipboard !== "string") throw new Error("Actual advisor clipboard boundary did not contain text");
      advisor.clipboardMatchesHistory = clipboard === advisor.transcript;
    }
    observation.advisor = advisor;
  }
  return observation;
}

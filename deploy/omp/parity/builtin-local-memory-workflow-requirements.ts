import type { BuiltinProviderTurn, BuiltinRequirements } from "./builtin-requirements";

const lesson = "PARITY_SQLITE_LESSON";
function learnTurns(prompt: string, callId: string): BuiltinProviderTurn[] {
  return [
    { match: { modelId: "parity-coder", textIncludes: prompt, purpose: "foreground" }, toolCalls: [{ id: callId, name: "learn", arguments: { memory: lesson, context: "Parity owned SQLite workflow." } }] },
    { match: { modelId: "parity-coder", textIncludes: prompt, purpose: "foreground" }, text: "PARITY_SQLITE_STORED" },
  ];
}

export function applyLocalMemoryWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  const lifecycle = requirements["builtin-memory-sqlite-durable-lifecycle"];
  const reset = requirements["builtin-memory-sqlite-reset"];
  const sharpshooter = requirements["builtin-memory-sharpshooter-extracted-queue"];
  if (!lifecycle || !reset || !sharpshooter) throw new Error("Missing real local-memory workflow cases");
  lifecycle.models.turns = [
    ...learnTurns("Store PARITY_SQLITE_LESSON as a durable local fact.", "parity-sqlite-learn"),
    { match: { modelId: "parity-coder", textIncludes: "Parity second local memory turn.", purpose: "foreground" }, text: "PARITY_SQLITE_SECOND" },
    { match: { modelId: "parity-coder", textIncludes: "Parity third local memory turn.", purpose: "foreground" }, text: "PARITY_SQLITE_THIRD" },
  ];
  reset.models.turns = learnTurns("Store PARITY_SQLITE_LESSON before reset.", "parity-sqlite-reset-learn");
  for (const requirement of [lifecycle, reset, sharpshooter]) {
    requirement.lifecycle.superviseRealRestart = true;
    requirement.lifecycle.resumeSameSession = true;
    requirement.prerequisites.push("Use actual configured memory backend and production database/queue APIs. All agent directories, database paths and scoped banks must remain inside this leg's fresh owned HOME; scrub MNEMOPI_* and HINDSIGHT_* overrides. No backend replacement, remote embeddings or provider requests.");
  }
  for (const requirement of [lifecycle, reset]) {
    requirement.prerequisites.push("memory.backend=mnemopi; actual native LearnTool must be enabled by autolearn.enabled and the real backend. mnemopi.noEmbeddings=true and llmMode=none prevent downloads and extraction-provider requests. Capture live state and independently reopened readonly canonical bank SQLite files, including rows and retained_through_user_turn metadata, not UI status alone.");
  }
  const rule = "Corrective rule: PARITY_DECISION is mandatory because silent fallback regresses owned settings.";
  sharpshooter.models.turns = [
    { match: { modelId: "parity-coder", textIncludes: rule, purpose: "foreground" }, text: "PARITY_SHARPSHOOTER_READY" },
    { match: { modelId: "parity-small", textIncludes: rule }, toolCalls: [{ id: "parity-record-decision", name: "record_deltas", arguments: { deltas: [{ kind: "architecture_decision", statement: "PARITY_DECISION is mandatory.", source: "explicit_user", evidence: "PARITY_DECISION is mandatory", friction: { corrective: true, regression: true, subtle: false } }] } }] },
    { match: { modelId: "parity-small", textIncludes: "PARITY_DECISION is mandatory." }, toolCalls: [{ id: "parity-consolidate-decision", name: "replace_memory_files", arguments: { files: [{ name: "architecture.md", content: "PARITY_DECISION is mandatory.\n" }, { name: "product.md", content: "" }, { name: "style.md", content: "" }] } }] },
  ];
  sharpshooter.prerequisites.push("Do not seed queue or memory files. The canonical Sharpshooter user-message subscription must issue its real parity-small record_deltas completion and append the admitted exact-evidence delta. Capture waits through real flushSharpshooterExtraction(session). Explicit /memory sync calls unchanged consolidation and consumes actual delta files only after real replace_memory_files output is applied. Keep positive queue checkpoint before the canonical 60-second scheduler tick; do not suppress or replace its scheduler.");
}

import type { BehaviorClaim, ComparisonReference } from "./scenario";
import type { InventoryEntry } from "./inventory-types";
import type { ProbeVisit } from "./probe";
import { equivalent, positiveRendering, type ExecutedCheckpoint, type ExecutedComparison, type ScenarioEvidence } from "./coverage";
import { provenanceReasons } from "./coverage-provenance";
import type { BehaviorProof } from "./coverage-behavior";

export interface LocatedBehavior { checkpoint: ExecutedCheckpoint; comparison: ExecutedComparison; index: number }
interface Context { scenario: ScenarioEvidence; current: LocatedBehavior; target: LocatedBehavior; locate(reference: ComparisonReference, through: number): LocatedBehavior }
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const location = (value: unknown): value is { source: string; line: number } => record(value) && typeof value.source === "string" && Number.isInteger(value.line);
const origins = (value: unknown): Array<{ source: string; line: number }> => !value || typeof value !== "object" ? []
  : [...(location(value) ? [value] : []), ...Object.values(value).flatMap(origins)];

/** Completed registry operations bind actual source owners, dispatched input and independent visible/host outcomes. */
export function actionBehaviorProof(entry: InventoryEntry, claim: BehaviorClaim, context: Context): BehaviorProof | undefined {
  if (!["command", "subcommand", "scene", "tool", "composer", "segment"].includes(entry.category)) return undefined;
  const { scenario, current, target, locate } = context;
  if (!["literal", "interaction", "host"].includes(claim.effect) || !positiveRendering(target.comparison)) throw new Error(`Action ${entry.id} requires a genuine current visible outcome and literal/interaction/host proof`);
  const visits: ProbeVisit[] = [];
  for (const adapter of ["terminal", "browser"] as const) {
    const cursor = current.checkpoint.probeCursors?.[adapter];
    if (!cursor) throw new Error(`Action ${entry.id} has no actual ${adapter} observation cursor`);
    visits.push(...scenario.visits.filter(row => row.adapter === adapter && row.pid === cursor.pid && row.generation === cursor.generation && row.sequence <= cursor.sequence));
  }
  const reasons = provenanceReasons(entry, visits, new Set());
  if (reasons.length) throw new Error(`Action ${entry.id} lacks its actual source execution: ${reasons.join("; ")}`);
  if (claim.effect === "literal") {
    if (current.comparison.terminal.type !== "screen" || (current.comparison.terminal.read ?? "capture") !== "capture"
      || current.comparison.browser.type !== "dom" || !["text", "value"].includes(current.comparison.browser.read)
      || !equivalent(current.comparison.terminalValue, claim.value) || !equivalent(current.comparison.browserValue, claim.value)
      || typeof claim.value !== "string" || !claim.value.trim()) throw new Error(`Action ${entry.id} literal must be the actual rendered status on both adapters`);
  } else {
    const direct = current.checkpoint.stimulus?.terminalInput && current.checkpoint.stimulus.browserActions.some(action => !["hover", "resize", "transportDelay", "reload", "emulateMedia"].includes(action.type));
    if (!direct && !(entry.category === "command" || entry.category === "subcommand") || !direct && !scenario.initialStimulus) throw new Error(`Action ${entry.id} has no actual dispatched input on both adapters`);
    if (!claim.assertionGroups?.length) throw new Error(`Action ${entry.id} requires explicit actual controller/host consequences`);
    const assertions = claim.assertionGroups.flatMap(group => group.comparisons.map(reference => locate(reference, current.index)));
    if (!assertions.some(item => item.index === current.index && ["effect", "file"].includes(item.comparison.terminal.type)
      && ["effect", "file"].includes(item.comparison.browser.type) && item.comparison.terminalValue !== null && item.comparison.terminalValue !== undefined)) throw new Error(`Action ${entry.id} cannot be proved by unrelated labels or wire presence`);
    if (!equivalent(current.comparison.terminalValue, claim.value) || !equivalent(current.comparison.browserValue, claim.value)) throw new Error(`Action ${entry.id} claim value differs from the independently observed actual outcome`);
  }
  const relevant = origins(entry.details);
  const source = visits.findLast(row => row.adapter === "browser" && relevant.some(origin => row.source === origin.source && (row.line === origin.line || row.ownerLine === origin.line)));
  if (!source) throw new Error(`Action ${entry.id} has no actual source owner to bind its outcome`);
  const terminal = visits.findLast(row => row.adapter === "terminal" && relevant.some(origin => row.source === origin.source && (row.line === origin.line || row.ownerLine === origin.line)));
  if (!terminal) throw new Error(`Action ${entry.id} has no actual TUI source owner`);
  return { entry: entry.id, value: claim.value, effect: claim.effect, checkpoint: current.checkpoint.name, comparison: current.comparison.name,
    source: { pid: source.pid, generation: source.generation!, sequence: source.sequence, pointer: "" },
    terminalSource: { pid: terminal.pid, generation: terminal.generation!, sequence: terminal.sequence, source: terminal.source, line: terminal.line },
    target: { surface: target.comparison.browserTarget?.surface ?? "", node: target.comparison.browserTarget?.node ?? "" } };
}

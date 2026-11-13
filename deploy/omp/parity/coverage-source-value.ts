import type { InventoryEntry } from "./inventory-types";
import type { ProbeDocument, ProbeVisit } from "./probe";
import type { ComparisonReference } from "./scenario";
import { equivalent, positiveRendering, type ExecutedCheckpoint, type ExecutedComparison, type ScenarioEvidence } from "./coverage";
import { WireSchemaEvidence, type PropertyWitness } from "./coverage-schema";

export const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
export const nonempty = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0;
export interface SourceLocation { source: string; line: number }
export const location = (value: unknown): value is SourceLocation => record(value) && nonempty(value.source) && Number.isSafeInteger(value.line) && Number(value.line) > 0;
export interface LocatedBehavior { checkpoint: ExecutedCheckpoint; comparison: ExecutedComparison; index: number }
export interface ReferenceProjection extends SourceLocation { semantic: unknown; wireId: string; selectedAfter: string; pid: number; generation: string; sequence: number }
export interface SourceValue { row: ProbeDocument; witness: PropertyWitness; projection?: ReferenceProjection }
export interface BehaviorContext {
  scenario: ScenarioEvidence; entries: ReadonlyMap<string, InventoryEntry>; current: LocatedBehavior; target: LocatedBehavior;
  locate(reference: ComparisonReference, through: number): LocatedBehavior;
  values(target: LocatedBehavior): { row: ProbeDocument; values: PropertyWitness[] };
  currentValue(target: LocatedBehavior, entry: InventoryEntry, value: unknown, live?: boolean): SourceValue;
}
export const sameLocation = (visit: SourceLocation & { ownerLine?: number }, origin: SourceLocation): boolean => visit.source === origin.source && (visit.line === origin.line || visit.ownerLine === origin.line);
export function currentVisits(scenario: ScenarioEvidence, target: LocatedBehavior, adapter: "terminal" | "browser", through?: number): ProbeVisit[] {
  const cursor = target.checkpoint.probeCursors?.[adapter];
  if (!cursor) throw new Error(`Missing actual ${adapter} observation cursor`);
  return scenario.visits.filter(row => row.adapter === adapter && row.pid === cursor.pid && row.generation === cursor.generation && row.sequence <= Math.min(cursor.sequence, through ?? cursor.sequence));
}
export function dispatchedInput(context: BehaviorContext): boolean {
  const stimulus = context.current.checkpoint.stimulus;
  return !!stimulus?.terminalInput && stimulus.browserActions.some(action => !["hover", "resize", "transportDelay", "reload", "emulateMedia"].includes(action.type));
}
export function assertions(context: BehaviorContext, groups: unknown): LocatedBehavior[] {
  if (!Array.isArray(groups) || !groups.length) throw new Error("Behavior requires explicit consequence assertion groups");
  return groups.flatMap(group => {
    if (!record(group) || !Array.isArray(group.comparisons) || !group.comparisons.length) throw new Error("Empty behavior assertion group");
    return group.comparisons.map(reference => context.locate(reference as ComparisonReference, context.current.index));
  });
}
export function sourceTerminal(entry: InventoryEntry, context: BehaviorContext, actual: SourceValue): ProbeVisit {
  const terminal = currentVisits(context.scenario, context.target, "terminal");
  const browser = currentVisits(context.scenario, context.target, "browser", actual.row.sequence);
  const emitters = Array.isArray(entry.details.uiEmitters) ? entry.details.uiEmitters.filter(record) : [];
  for (const emitter of emitters) {
    if (!location(emitter.nativeHook) || !browser.some(visit => sameLocation(visit, emitter.nativeHook as SourceLocation))) continue;
    const path = Array.isArray(emitter.path) ? emitter.path.filter(record) : [];
    if (!path.every(edge => location(edge.target) && browser.some(visit => sameLocation(visit, edge.target as SourceLocation)))) continue;
    for (const hook of Array.isArray(emitter.terminalHooks) ? emitter.terminalHooks : []) {
      if (!location(hook)) continue;
      const visit = terminal.findLast(visit => sameLocation(visit, hook) && /^render/.test(visit.method)
        && (!nonempty(emitter.component) || visit.component === emitter.component.slice("component:".length) || visit.receiverComponent === emitter.component.slice("component:".length)));
      if (visit) return visit;
    }
  }
  throw new Error(`Behavior ${entry.id} lacks same-generation actual TUI rendering and its source-derived native UI producer`);
}

/** Canonical HTML documents prove ownership only, and only at the exact observed frame. */
export function sourceValueReader(scenario: ScenarioEvidence, reader: WireSchemaEvidence): Pick<BehaviorContext, "values" | "currentValue"> {
  const parsed = new Map<ProbeDocument, PropertyWitness[]>();
  const values = (target: LocatedBehavior) => {
    const dom = target.comparison.browserTarget, cursor = target.checkpoint.probeCursors?.browser;
    if (!dom?.visible || !dom.node || !dom.surface || !Number.isSafeInteger(dom.surfaceSequence) || !cursor) throw new Error("Behavior requires a current visible typed HTML owner and actual cursor");
    const row = (scenario.documents ?? []).filter(row => row.adapter === "browser" && row.pid === cursor.pid && row.generation === cursor.generation
      && row.sequence <= cursor.sequence && row.surface === dom.surface && row.surfaceSequence === dom.surfaceSequence).sort((first, second) => second.sequence - first.sequence)[0];
    if (!row) throw new Error("Behavior has no exact-current canonical HTML mirror frame");
    let witnesses = parsed.get(row);
    if (!witnesses) { witnesses = reader.document(row).values; parsed.set(row, witnesses); }
    return { row, values: witnesses.filter(witness => witness.surface === dom.surface && (witness.node === dom.node
      || dom.owners?.some(owner => owner.surface === witness.surface && owner.node === witness.node))) };
  };
  const currentValue = (target: LocatedBehavior, entry: InventoryEntry, value: unknown, live = false): SourceValue => {
    const actual = values(target);
    const property = entry.category === "property" || entry.category === "kind" ? entry.id : entry.details.propertyId;
    for (const witness of actual.values.filter(witness => witness.entry === property)) {
      if (live ? typeof witness.value === "number" && Number.isFinite(witness.value) : equivalent(witness.value, value)) return { row: actual.row, witness };
      const projection = entry.details.valueProjection;
      const output = record(projection) ? projection.output : undefined;
      if (!location(output) || typeof witness.value !== "string") continue;
      const relation = (scenario.raw ?? []).findLast(row => row.type === "reference" && row.adapter === "browser" && row.pid === actual.row.pid
        && row.generation === actual.row.generation && Number.isSafeInteger(row.sequence) && Number(row.sequence) <= actual.row.sequence
        && row.source === output.source && row.line === output.line && equivalent(row.semantic, value)
        && row.wireId === witness.value && row.selectedAfter === witness.value);
      if (relation) return { row: actual.row, witness, projection: relation as unknown as ReferenceProjection };
    }
    throw new Error(`Behavior ${entry.id} value is absent from the exact current typed owner`);
  };
  return { values, currentValue };
}

export function requireVisible(target: LocatedBehavior): void {
  if (!positiveRendering(target.comparison)) throw new Error("Behavior requires independent current TTY text and visible DOM, never wire presence or a negative UI case");
}

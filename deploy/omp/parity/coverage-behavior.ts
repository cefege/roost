import type { BehaviorClaim, ComparisonReference, ContentPart } from "./scenario";
import type { InventoryEntry } from "./inventory-types";
import type { ProbeDocument } from "./probe";
import { equivalent, positiveRendering, type ExecutedCheckpoint, type ExecutedComparison, type ScenarioEvidence } from "./coverage";
import { WireSchemaEvidence, type PropertyWitness } from "./coverage-schema";
import { actionBehaviorProof } from "./coverage-action";
import { settingsBehaviorProof } from "./coverage-settings";
import { sourceTerminal, sourceValueReader, type BehaviorContext } from "./coverage-source-value";
import { pointerValue } from "./observation-pointer";

export interface BehaviorProof {
  entry: string; value: unknown; effect: BehaviorClaim["effect"]; checkpoint: string; comparison: string;
  source: { pid: number; generation: string; sequence: number; pointer: string };
  terminalSource: { pid: number; generation: string; sequence: number; source: string; line: number };
  target: { surface: string; node: string };
}
interface Located { checkpoint: ExecutedCheckpoint; comparison: ExecutedComparison; index: number }
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const nonempty = (value: unknown): value is string => typeof value === "string" && value.trim().length > 0;
const layoutScreen = ["column", "row", "leftHalf", "rightHalf", "topHalf", "bottomHalf", "rightGap", "bottomGap", "sameRow", "rowGap", "columnGap", "below"];
const layoutDOM = ["rectWidth", "rectHeight", "rectX", "rectY", "leftHalf", "rightHalf", "topHalf", "bottomHalf", "rightGap", "bottomGap", "scrollTop", "scrollHeight", "clientHeight", "sameRow", "rowGap", "columnGap", "below"];

/** Source snapshots establish only typed ownership; actual screen/DOM/effects establish behavior. */
export function behaviorProofs(entries: ReadonlyMap<string, InventoryEntry>, scenario: ScenarioEvidence, reader: WireSchemaEvidence): BehaviorProof[] {
  const proofs: BehaviorProof[] = [];
  const parsed = new Map<ProbeDocument, PropertyWitness[]>();
  const locate = (reference: ComparisonReference, through: number): Located => {
    if (!record(reference) || !nonempty(reference.checkpoint) || !nonempty(reference.comparison)) throw new Error("Malformed behavior comparison reference");
    const index = scenario.checkpoints.findIndex(checkpoint => checkpoint.name === reference.checkpoint);
    const checkpoint = scenario.checkpoints[index];
    const comparison = checkpoint?.comparisons.find(item => item.name === reference.comparison);
    if (index < 0 || index > through || !checkpoint?.passed || !comparison?.passed || comparison.expectedByAdapter) throw new Error("Behavior claims require actual positive comparisons at or before their checkpoint");
    return { checkpoint, comparison, index };
  };
  const currentValue = (located: Located, entry: string, value: unknown, live = false): { row: ProbeDocument; witness: PropertyWitness } => {
    const target = located.comparison.browserTarget;
    const cursor = located.checkpoint.probeCursors?.browser;
    if (!target?.node || !target.surface || !Number.isSafeInteger(target.surfaceSequence) || !cursor || !positiveRendering(located.comparison)) throw new Error(`Behavior ${entry} requires actual visible DOM owner, folded surface sequence and current terminal match provenance`);
    const row = (scenario.documents ?? []).filter(item => item.adapter === "browser" && item.pid === cursor.pid && item.generation === cursor.generation
      && item.sequence <= cursor.sequence && item.surface === target.surface && item.surfaceSequence === target.surfaceSequence).sort((first, second) => second.sequence - first.sequence)[0];
    if (!row) throw new Error(`Behavior ${entry} has no current canonical source-owned document snapshot`);
    let values = parsed.get(row);
    if (!values) { values = reader.document(row).values; parsed.set(row, values); }
    const witness = values.find(item => item.entry === entry && item.node === target.node && item.surface === target.surface
      && (live ? typeof item.value === "number" && Number.isFinite(item.value) : equivalent(item.value, value)));
    if (!witness) throw new Error(`Behavior ${entry} value is absent from the actual typed current owner`);
    return { row, witness };
  };
  const sourceValues = sourceValueReader(scenario, reader);
  for (const [index, checkpoint] of scenario.checkpoints.entries()) for (const comparison of checkpoint.comparisons) {
    if (comparison.proves !== undefined && !Array.isArray(comparison.proves)) throw new Error("Behavior proves must be an explicit claim array");
    for (const claim of comparison.proves ?? []) {
      if (!record(claim) || !nonempty(claim.entry) || claim.effect !== "temporal" && !Object.hasOwn(claim, "value") || !["literal", "appearance", "layout", "interaction", "host", "temporal", "link", "content"].includes(claim.effect)) throw new Error("Malformed explicit observable behavior claim");
      const entry = entries.get(claim.entry);
      if (!entry || !scenario.covers.includes(entry.id)) throw new Error(`Behavior claim ${claim.entry} must name a source inventory entry explicitly covered by this scenario`);
      if (comparison.expectedByAdapter || comparison.temporal?.direction === "sample") throw new Error("Negative outcomes and timer baseline samples cannot prove behavior");
      const own: Located = { checkpoint, comparison, index };
      const target = claim.target ? locate(claim.target, index) : own;
      if (target.index !== index) throw new Error("Behavior target presence must be observed in the same checkpoint");
      const action = actionBehaviorProof(entry, claim, { scenario, current: own, target, locate });
      if (action) { proofs.push(action); continue; }
      const context = { scenario, entries, current: own, target, locate, ...sourceValues } satisfies BehaviorContext;
      const setting = settingsBehaviorProof(entry, claim, context);
      if (setting) { proofs.push(setting); continue; }
      const propertyId = entry.category === "property" ? entry.id : typeof entry.details.propertyId === "string" ? entry.details.propertyId : undefined;
      if (!propertyId) throw new Error(`Behavior ${entry.id} has no retained source property identity`);
      const actual = currentValue(target, propertyId, claim.value, claim.effect === "temporal");
      let before: Located | undefined;
      if (claim.before) {
        before = locate(claim.before, index - 1);
        const prior = before.comparison.proves?.find(item => item.entry === entry.id);
        if (!prior || equivalent(prior.value, claim.value)) throw new Error(`Behavior ${entry.id} transition requires its own earlier distinct source value`);
        const priorTarget = prior.target ? locate(prior.target, before.index) : before;
        currentValue(priorTarget, propertyId, prior.value);
        if (equivalent(before.comparison.terminalValue, comparison.terminalValue) || equivalent(before.comparison.browserValue, comparison.browserValue)) throw new Error(`Behavior ${entry.id} transition did not change both actual observations`);
      }
      if (claim.effect === "temporal") {
        if (!comparison.temporal) throw new Error(`Temporal ${entry.id} requires actual per-adapter rendered numeric movement`);
      } else if (claim.effect === "content") {
        if (!record(claim.value) && !Array.isArray(claim.value)) throw new Error(`Content ${entry.id} must claim a structured source value`);
        const leaves = scalarLeaves(claim.value);
        const parts = claim.parts;
        if (!leaves.length || !Array.isArray(parts) || parts.length !== leaves.length
          || leaves.some(pointer => parts.filter(part => record(part) && part.pointer === pointer).length !== 1)) throw new Error(`Content ${entry.id} needs exactly one part per scalar leaf of its value`);
        for (const part of parts) {
          const shown = locate(part.comparison, index);
          const owner = shown.comparison.browserTarget;
          if (shown.index !== index || owner?.surface !== target.comparison.browserTarget?.surface || owner?.node !== target.comparison.browserTarget?.node) throw new Error(`Content ${entry.id} part ${part.pointer} must be observed in this checkpoint inside the owner holding the value`);
          observableEffect(part.effect, `${entry.id}${part.pointer}`, shown.comparison, pointerValue(claim.value, part.pointer), undefined);
        }
      } else if (["link", "literal", "appearance", "layout"].includes(claim.effect)) {
        observableEffect(claim.effect as ContentPart["effect"], entry.id, comparison, claim.value, before);
      } else {
        const groups = claim.assertionGroups;
        if (!Array.isArray(groups) || !groups.length) throw new Error(`Behavioral ${entry.id} needs explicit state and consequence assertion groups`);
        if (!checkpoint.stimulus?.terminalInput || !checkpoint.stimulus.browserActions.some(action => !["hover", "resize", "transportDelay", "reload", "emulateMedia"].includes(action.type))) throw new Error(`Behavioral ${entry.id} needs genuine dispatched input on both adapters`);
        const assertions = groups.flatMap(group => {
          if (!record(group) || !Array.isArray(group.comparisons) || !group.comparisons.length) throw new Error("Empty behavior assertion group");
          return group.comparisons.map(reference => locate(reference, index));
        });
        if (!assertions.some(item => item.index === index && (item.comparison.terminal.type === "effect" || item.comparison.terminal.type === "file"
          || item.comparison.browser.type === "dom" && ["value", "checked", "selectionStart", "selectionEnd"].includes(item.comparison.browser.read)))) throw new Error(`Behavioral ${entry.id} has no actual controller, host or input-state consequence`);
        if (!before) {
          const leaf = String(entry.details.path ?? propertyId.slice(propertyId.lastIndexOf(".") + 1)).split(".").at(-1)!;
          const state = target.comparison.browserTarget?.state;
          const stateKey = leaf === "busy" ? "disabled" : leaf;
          const semantic = typeof claim.value === "boolean" && state && Object.hasOwn(state, stateKey) && state[stateKey] === claim.value;
          const actualEffect = assertions.some(item => item.index === index && ["effect", "file"].includes(item.comparison.terminal.type) && ["effect", "file"].includes(item.comparison.browser.type)
            && item.comparison.terminalValue !== null && item.comparison.terminalValue !== undefined && item.comparison.browserValue !== null && item.comparison.browserValue !== undefined);
          if (!semantic || !actualEffect) throw new Error(`Behavioral ${entry.id} needs actual matching semantic DOM state plus real controller/host result, or a controlled source-value transition`);
        }
      }
      const terminal = sourceTerminal(entry, context, actual);
      proofs.push({ entry: entry.id, value: actual.witness.value, effect: claim.effect, checkpoint: checkpoint.name, comparison: comparison.name,
        source: { pid: actual.row.pid, generation: actual.row.generation!, sequence: actual.row.sequence, pointer: actual.witness.pointer },
        terminalSource: { pid: terminal.pid, generation: terminal.generation!, sequence: terminal.sequence, source: terminal.source, line: terminal.line },
        target: { surface: target.comparison.browserTarget!.surface!, node: target.comparison.browserTarget!.node! } });
    }
  }
  return proofs;
}

/** Every scalar (non-container) position in a structured value, as RFC 6901 pointers. */
function scalarLeaves(value: unknown, pointer = ""): string[] {
  const children: Array<[string, unknown]> = Array.isArray(value) ? value.map((item, idx) => [String(idx), item]) : record(value) ? Object.entries(value) : [];
  if (!Array.isArray(value) && !record(value)) return [pointer];
  return children.flatMap(([key, item]) => scalarLeaves(item, `${pointer}/${key.replaceAll("~", "~0").replaceAll("/", "~1")}`));
}

/** A screen/DOM pair proves one scalar only when both adapters read it through the effect's observable channel. */
function observableEffect(effect: ContentPart["effect"], label: string, comparison: ExecutedComparison, value: unknown, before: Located | undefined): void {
  if (effect === "link") {
    if (comparison.terminal.type !== "ansi" || comparison.browser.type !== "dom" || comparison.browser.read !== "attribute"
      || comparison.browser.attribute !== "href" || !nonempty(value) || !equivalent(comparison.terminalValue, value)
      || !equivalent(comparison.browserValue, value)) throw new Error(`Link ${label} requires actual OSC8 href and real native DOM href plus a separate visible current target`);
  } else if (effect === "literal") {
    if (!(typeof value === "string" && value.trim() || typeof value === "number" && Number.isFinite(value))
      || !positiveRendering(comparison) || comparison.terminal.type !== "screen" || (comparison.terminal.read ?? "capture") !== "capture"
      || comparison.browser.type !== "dom" || !["text", "value", "attribute", "naturalWidth", "naturalHeight"].includes(comparison.browser.read)
      || !equivalent(comparison.terminalValue, value) || !equivalent(comparison.browserValue, value)) throw new Error(`Literal ${label} must read its exact source scalar from actual current screen and DOM`);
  } else if (effect === "appearance") {
    const paint = comparison.terminal.type === "screen" && ["foreground", "background"].includes(comparison.terminal.read ?? "")
      && comparison.browser.type === "dom" && comparison.browser.read === "css" && nonempty(comparison.browser.property)
      && nonempty(comparison.terminalValue) && nonempty(comparison.browserValue);
    const style = comparison.terminal.type === "screen" && ["bold", "italic", "underline", "inverse"].includes(comparison.terminal.read ?? "")
      && comparison.browser.type === "dom" && comparison.browser.read === comparison.terminal.read
      && comparison.terminalValue === true && comparison.browserValue === true;
    if ((!paint && !style) || !before && !Object.hasOwn(comparison, "expected")) throw new Error(`Appearance ${label} needs actual paint/style reads and a source-specific expected value or transition`);
  } else if (comparison.terminal.type !== "screen" || !layoutScreen.includes(comparison.terminal.read ?? "")
    || comparison.browser.type !== "dom" || !layoutDOM.includes(comparison.browser.read)
    || !before && !Object.hasOwn(comparison, "expected")
    || comparison.terminalValue === null || comparison.terminalValue === undefined || !equivalent(comparison.terminalValue, comparison.browserValue)) throw new Error(`Layout ${label} needs actual current cell/DOM geometry measured equal on both adapters, and an explicit constraint or transition`);
}

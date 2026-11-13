import type { BrowserAction, Comparison, Observation } from "./scenario";
import type { InterfaceInventory, InventoryEntry } from "./inventory-types";
import type { ProbeDocument, ProbeVisit, WireObservation } from "./probe";
import { provenanceReasons } from "./coverage-provenance";
import { WireSchemaEvidence } from "./coverage-schema";
import { temporalSatisfied } from "./coverage-temporal";
import { behaviorProofs, type BehaviorProof } from "./coverage-behavior";
import { nativeContractProofs, type ExecutedNativeContract, type NativeContractProof } from "./coverage-native";
import { coverageLane, sourceAudit, type CoverageLane, type SourceAudit } from "./coverage-lanes";
import { nullDomainReasons } from "./coverage-domain";
import { relayOwnedValues } from "./coverage-relay";

export interface ProbeCursor { pid: number; generation: string; sequence: number }
export interface BrowserTarget {
  visible: boolean; surface?: string; surfaceSequence?: number; node?: string; attributes: Record<string, string>; state: Record<string, boolean>;
  identity?: Partial<Record<"setting" | "control" | "page" | "pageTab" | "pageActive" | "item" | "itemSelected", string>>;
  owners?: Array<{ node: string; surface: string }>;
  prefs?: { page?: string; tab?: string; rows: string[] };
}
export interface ExecutedComparison extends Comparison {
  passed: boolean; terminalValue: unknown; browserValue: unknown;
  terminalRawValue?: unknown; browserRawValue?: unknown;
  terminalMatch?: { text: string };
  browserTarget?: BrowserTarget;
}
export interface ExecutedCheckpoint {
  name: string; passed: boolean; comparisons: ExecutedComparison[];
  probeCursors?: { terminal: ProbeCursor; browser: ProbeCursor };
  stimulus?: { terminalInput?: string; browserActions: BrowserAction[] };
  nativeContracts?: ExecutedNativeContract[];
}
export interface ScenarioEvidence {
  id: string; covers: string[]; checkpoints: ExecutedCheckpoint[]; visits: ProbeVisit[]; wire: WireObservation[];
  documents?: ProbeDocument[];
  raw?: Record<string, unknown>[];
  initialStimulus?: { terminalInput: string; browser: { role: "textbox"; name: string; value: string; key: string } };
}
export interface EntryCoverage { id: string; category: string; source: string; line: number; lane: CoverageLane; coveredBy: string[]; missingEvidence: string[]; obligations: string[]; behavior: BehaviorProof[] }
export interface CoverageReport {
  revision: string; inventoryTotal: number; total: number; covered: number; entries: EntryCoverage[]; invalid: string[];
  nativeContractConformance: { total: number; conformant: number; proofs: NativeContractProof[] };
  sourceAudit: { total: number; entries: SourceAudit[] };
}
export class CoverageFailure extends Error {
  constructor(readonly report: CoverageReport) {
    super(`OMP parity coverage failed: ${report.total - report.covered}/${report.total} UI entries missing evidence; ${report.nativeContractConformance.total - report.nativeContractConformance.conformant} native contracts missing conformance; ${report.invalid.length} invalid evidence records.\n${[...report.invalid, ...report.entries.filter(entry => entry.missingEvidence.length).map(entry => `${entry.id}: ${entry.missingEvidence.join("; ")}`)].join("\n")}`);
  }
}
const owns = (value: object, name: string) => Object.prototype.hasOwnProperty.call(value, name);
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.length > 0;
const sourcePath = (value: unknown): value is string => text(value) && value.startsWith("packages/") && !value.split("/").includes("..");
const categories = ["kind", "property", "verb", "event", "operation", "query", "reply", "component", "feature", "feature-check", "command", "subcommand", "setting", "setting-choice", "setting-condition", "setting-options-factory", "setting-options-state", "settings-tab", "control", "tool", "composer", "segment", "scene"];
// Categories whose claims carry an explicit `proves` behavior proof (coverage-behavior,
// -action, -settings). Components, kinds, wire verbs/events and features are proven by
// actual production execution (coverage-provenance) plus a positive current rendering.
const explicitlyProven = ["property", "command", "subcommand", "scene", "tool", "composer", "segment", "setting", "control", "setting-choice", "setting-condition", "setting-options-factory", "setting-options-state"];

/** Rechecks normalized observations rather than trusting covers or passed booleans. */
export function assertCoverage(inventory: InterfaceInventory, evidence: ScenarioEvidence[]): CoverageReport {
  if (!record(inventory) || !text(inventory.revision) || !Array.isArray(inventory.entries) || !inventory.entries.length) throw new Error("A nonempty source-derived interface inventory is required");
  if (!record(inventory.runtimeSchema) || !Object.keys(inventory.runtimeSchema).length) throw new Error("The source-derived runtime schema graph is required");
  if (!Array.isArray(evidence) || !evidence.length) throw new Error("Executed scenario evidence is required");
  const entries = new Map<string, InventoryEntry>();
  for (const entry of inventory.entries) {
    if (!record(entry) || !text(entry.id) || !text(entry.category) || !entry.id.startsWith(`${entry.category}:`) || !sourcePath(entry.source) || !Number.isInteger(entry.line) || entry.line < 1 || !record(entry.details)) throw new Error("Malformed source inventory entry");
    if (entries.has(entry.id)) throw new Error(`Duplicate inventory entry ${entry.id}`);
    entries.set(entry.id, entry);
  }
  const lanes = new Map([...entries.values()].map(entry => [entry.id, coverageLane(entry)]));
  const audits = [...entries.values()].flatMap(entry => sourceAudit(entry) ?? []);
  const report: CoverageReport = { revision: inventory.revision, inventoryTotal: entries.size, total: [...lanes.values()].filter(lane => lane === "tui-html-parity").length,
    covered: 0, entries: [], invalid: [], nativeContractConformance: { total: [...lanes.values()].filter(lane => lane === "native-contract-conformance").length, conformant: 0, proofs: [] },
    sourceAudit: { total: audits.length, entries: audits } };
  const reader = new WireSchemaEvidence(inventory.runtimeSchema);
  const scenarios = new Set<string>();
  const claims = new Map<string, Array<{ id: string; visits: ProbeVisit[]; facts: Set<string>; behavior: BehaviorProof[] }>>();
  const observedRows = new Set<string>();
  const rawRowsSeen = new Set<string>();
  for (const scenario of evidence) {
    try {
      if (!record(scenario) || !text(scenario.id) || scenarios.has(scenario.id)) throw new Error("Missing or duplicate scenario identity");
      scenarios.add(scenario.id);
      if (!Array.isArray(scenario.covers) || new Set(scenario.covers).size !== scenario.covers.length || scenario.covers.some(id => !entries.has(id))) throw new Error("Missing, duplicate or unknown covers entries");
      if (scenario.covers.some(id => lanes.get(id) !== "tui-html-parity")) throw new Error("Native-only/source-audit inventory cannot be claimed as TUI/HTML parity");
      if (!Array.isArray(scenario.checkpoints) || !scenario.checkpoints.length || !Array.isArray(scenario.visits) || !Array.isArray(scenario.wire)) throw new Error("Executed checkpoints and raw visit/wire arrays are required");
      let meaningful = false;
      let currentRendering = false;
      const previousCheckpoints: ExecutedCheckpoint[] = [];
      const checkpointNames = new Set<string>();
      for (const checkpoint of scenario.checkpoints) {
        if (!record(checkpoint) || !text(checkpoint.name) || checkpointNames.has(checkpoint.name) || checkpoint.passed !== true || !Array.isArray(checkpoint.comparisons)
          || !checkpoint.comparisons.length && !(Array.isArray(checkpoint.nativeContracts) && checkpoint.nativeContracts.length)) throw new Error("Malformed, duplicate, failed or empty executed checkpoint");
        checkpointNames.add(checkpoint.name);
        const comparisonNames = new Set<string>();
        for (const comparison of checkpoint.comparisons) {
          if (!record(comparison) || !text(comparison.name) || comparisonNames.has(comparison.name) || comparison.passed !== true || !owns(comparison, "terminalValue") || !owns(comparison, "browserValue")) throw new Error("Malformed, duplicate or failed executed comparison");
          comparisonNames.add(comparison.name);
          if (comparison.normalize !== undefined && !["whitespace", "number", "fixture-path", "percent-fraction", "percent-remaining-fraction"].includes(comparison.normalize)) throw new Error("Unsupported comparison normalization");
          validateObservation(comparison.terminal, "terminal");
          validateObservation(comparison.browser, "browser");
          validateNormalizedObservation(comparison);
          if (comparison.expectedByAdapter !== undefined) {
            if (owns(comparison, "expected")) throw new Error("A negative adapter outcome cannot also declare positive parity");
            const expected = comparison.expectedByAdapter;
            if (!record(expected) || !["native-descriptor-failure", "unsupported-interactive-component"].includes(expected.reason) || !owns(expected, "terminal") || !owns(expected, "browser")) throw new Error("Malformed deliberate negative adapter outcome");
            if (!equivalent(comparison.terminalValue, expected.terminal) || !equivalent(comparison.browserValue, expected.browser)) throw new Error(`Negative adapter outcomes did not match actual observations: ${comparison.name}`);
          } else {
            if (comparison.temporal ? !temporalSatisfied(comparison, previousCheckpoints) : !equivalent(comparison.terminalValue, comparison.browserValue)) throw new Error(`Actual normalized observations differ: ${comparison.name}`);
            if (owns(comparison, "expected") && (!equivalent(comparison.terminalValue, comparison.expected) || !equivalent(comparison.browserValue, comparison.expected))) throw new Error(`Actual observations do not match expected value: ${comparison.name}`);
            const observed = meaningfulValue(comparison.terminalValue) && meaningfulValue(comparison.browserValue);
            meaningful ||= observed;
            currentRendering ||= positiveRendering(comparison);
          }
        }
        previousCheckpoints.push(checkpoint);
      }
      if (scenario.covers.length && !meaningful) throw new Error("Coverage claims require a positive meaningful comparison, not null-only observations or expected adapter failures");
      if (!currentRendering && scenario.covers.some(id => ["kind", "component"].includes(entries.get(id)!.category))) throw new Error("Visible kind/component coverage requires a positive current terminal screen and browser DOM comparison");
      const facts = new Set<string>();
      for (const visit of scenario.visits) {
        validateRow(visit, "visit", observedRows);
        if (!text(visit.method) || !text(visit.component) || !visit.component.startsWith(`${visit.source}#`)) throw new Error("Malformed actual component visit provenance");
        if (visit.receiverComponent !== undefined && !entries.has(`component:${visit.receiverComponent}`)) throw new Error("Unknown receiver component identity");
      }
      if (scenario.raw !== undefined) {
        if (!Array.isArray(scenario.raw)) throw new Error("Malformed complete raw provenance");
        for (const row of scenario.raw) validateRow(row as unknown as WireObservation, String(row.type), rawRowsSeen);
        // Rows are unique per producer sequence (validateRow), so one keyed lookup replaces a scan of every raw row.
        const rawByProducerSequence = new Map(scenario.raw.map(raw => [`${raw.pid}:${raw.generation}:${raw.sequence}`, raw]));
        for (const row of [...scenario.visits, ...scenario.wire, ...(scenario.documents ?? [])]) {
          const raw = rawByProducerSequence.get(`${row.pid}:${row.generation}:${row.sequence}`);
          if (raw === undefined || !equivalent(raw, row)) throw new Error("Filtered production provenance differs from the retained raw source row");
        }
      }
      for (const row of scenario.wire) {
        validateRow(row, "wire", observedRows);
        if (row.direction !== "out" && row.direction !== "in" || !text(row.verb) || !owns(row, "body")) throw new Error("Malformed raw wire observation");
        const observed = reader.observe(row);
        if (row.adapter === "browser") for (const fact of observed) facts.add(fact);
      }
      if (scenario.documents !== undefined && !Array.isArray(scenario.documents)) throw new Error("Malformed canonical document observation array");
      for (const row of scenario.documents ?? []) {
        validateRow(row, "document", observedRows);
        if (row.source !== "packages/tui/src/native/backend.ts" || !text(row.surface) || !text(row.backend)
          || !Number.isSafeInteger(row.surfaceSequence) || row.surfaceSequence < 1 || !record(row.mirrorOrigin)
          || row.mirrorOrigin.source !== row.source || row.mirrorOrigin.method !== "constructor:mirror") throw new Error("Malformed actual canonical debug-mirror document provenance");
        reader.document(row);
      }
      for (const checkpoint of scenario.checkpoints) if (checkpoint.probeCursors) {
        for (const adapter of ["terminal", "browser"] as const) {
          const cursor = checkpoint.probeCursors[adapter];
          const rows = scenario.raw ?? [...scenario.visits, ...scenario.wire, ...(scenario.documents ?? [])];
          if (!record(cursor) || !rows.some(row => row.adapter === adapter && row.pid === cursor.pid && row.generation === cursor.generation && row.sequence === cursor.sequence)) throw new Error(`Checkpoint ${checkpoint.name} cursor is not an actual ${adapter} raw provenance row`);
        }
      }
      const behavior = behaviorProofs(entries, scenario, reader);
      report.nativeContractConformance.proofs.push(...nativeContractProofs(entries, scenario, reader));
      for (const proof of behavior) facts.add(proof.entry);
      for (const id of scenario.covers) {
        const entry = entries.get(id)!;
        if (explicitlyProven.includes(entry.category) && !behavior.some(proof => proof.entry === entry.id)) throw new Error(`Claimed ${id} without actual relevant production execution and explicit observable behavior proof`);
      }
      for (const id of scenario.covers) {
        const previous = claims.get(id) ?? [];
        previous.push({ id: scenario.id, visits: scenario.visits, facts, behavior: behavior.filter(proof => proof.entry === id) });
        claims.set(id, previous);
      }
    } catch (error) {
      report.invalid.push(`${record(scenario) && text(scenario.id) ? scenario.id : "<malformed scenario>"}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  for (const entry of entries.values()) {
    const lane = lanes.get(entry.id)!;
    if (lane !== "tui-html-parity") {
      const proofs = report.nativeContractConformance.proofs.filter(proof => proof.entry === entry.id);
      const missingEvidence = lane === "source-audit" ? [] : nativeConformanceReasons(entry, proofs);
      report.entries.push({ id: entry.id, category: entry.category, source: entry.source, line: entry.line, lane, coveredBy: proofs.map(proof => proof.scenario), missingEvidence, obligations: [], behavior: [] });
      if (lane === "native-contract-conformance" && !missingEvidence.length) report.nativeContractConformance.conformant++;
      continue;
    }
    const contributors = claims.get(entry.id) ?? [];
    const visits = contributors.flatMap(item => item.visits);
    const facts = new Set(contributors.flatMap(item => [...item.facts]));
    const reasons = !categories.includes(entry.category) ? [`Unsupported inventory category ${entry.category}`]
      : !contributors.length ? ["No executed scenario claims this entry"] : provenanceReasons(entry, visits, facts, contributors.map(item => item.visits));
    const behavior = contributors.flatMap(item => item.behavior);
    // A value every typed producer site excludes is audited from source (`unproducedValues`), not observed.
    const unproduced = record(entry.details.unproducedValues) && Array.isArray(entry.details.unproducedValues.values) && Array.isArray(entry.details.unproducedValues.typedSites)
      && entry.details.unproducedValues.typedSites.length > 0 ? entry.details.unproducedValues.values : [];
    for (const value of Array.isArray(entry.details.enumValues) ? entry.details.enumValues.filter(value => !unproduced.some(item => equivalent(item, value))) : []) {
      if (!behavior.some(proof => equivalent(proof.value, value))) reasons.push(`No explicit observable behavior for source enum value ${JSON.stringify(value)}`);
    }
    reasons.push(...nullDomainReasons(entry));
    if (record(entry.details.enumDomain) && entry.details.enumDomain.unresolved === true) reasons.push("Unresolved source property value domain");
    report.entries.push({ id: entry.id, category: entry.category, source: entry.source, line: entry.line, lane,
      coveredBy: reasons.length ? [] : contributors.map(item => item.id), missingEvidence: reasons,
      obligations: Array.isArray(entry.details.obligations) ? entry.details.obligations.map(String) : [], behavior });
    if (!reasons.length) report.covered++;
  }
  if (report.invalid.length || report.covered !== report.total || report.nativeContractConformance.conformant !== report.nativeContractConformance.total) throw new CoverageFailure(report);
  return report;
}

function validateRow(row: ProbeVisit | WireObservation | ProbeDocument, type: string, seen: Set<string>): void {
  if (!record(row) || row.type !== type || !sourcePath(row.source) || !Number.isInteger(row.line) || row.line < 1 || !Number.isInteger(row.pid) || row.pid < 1 || !Number.isInteger(row.sequence) || row.sequence < 1 || !["terminal", "browser"].includes(String(row.adapter)) || !text(row.generation)) throw new Error("Missing/malformed actual process, adapter or source provenance");
  const identity = `${row.pid}:${row.generation}:${row.sequence}`;
  if (seen.has(identity)) throw new Error(`Duplicate observed provenance row ${identity}`);
  seen.add(identity);
}

export function validateObservation(value: Observation, side: "terminal" | "browser"): void {
  if (!record(value) || !text(value.type)) throw new Error("Missing actual observation descriptor");
  const observationType: string = value.type;
  if (value.type === "screen" || value.type === "ansi") {
    if (side !== "terminal" || !text(value.pattern) || value.flags !== undefined && typeof value.flags !== "string"
      || value.group !== undefined && (!Number.isInteger(value.group) || value.group < 0)) throw new Error("Invalid terminal screen/ANSI observation");
    if (value.type === "screen") {
      const reads = ["capture", "column", "row", "leftHalf", "rightHalf", "topHalf", "bottomHalf", "visible", "rightGap", "bottomGap", "foreground", "background", "bold", "italic", "underline", "inverse", "sameRow", "rowGap", "columnGap", "below", "sameRight", "sameColumn"];
      if (value.read !== undefined && !reads.includes(value.read)) throw new Error("Unsupported terminal screen read");
      if (["sameRow", "rowGap", "columnGap", "below", "sameRight", "sameColumn"].includes(String(value.read)) && !record(value.relativeTo)) throw new Error("Relative terminal geometry requires an actual reference regex");
      if (value.relativeTo !== undefined) {
        if (!record(value.relativeTo) || !text(value.relativeTo.pattern) || value.relativeTo.flags !== undefined && typeof value.relativeTo.flags !== "string") throw new Error("Invalid terminal reference regex");
        new RegExp(value.relativeTo.pattern, value.relativeTo.flags);
      }
    } else if (owns(value, "read")) throw new Error("ANSI history observations have no screen read");
    new RegExp(value.pattern, value.flags);
  } else if (value.type === "dom") {
    const reads = ["text", "value", "attribute", "css", "count", "checked", "selectionStart", "selectionEnd", "selectionDirection", "rectWidth", "rectHeight", "rectX", "rectY", "scrollTop", "scrollHeight", "clientHeight", "leftHalf", "rightHalf", "topHalf", "bottomHalf", "insideViewport", "visible", "rightGap", "bottomGap", "bold", "italic", "underline", "inverse", "selected", "pressed", "expanded", "collapsed", "focused", "readonly", "disabled", "mask", "sameRow", "rowGap", "columnGap", "below", "sameRight", "sameColumn", "naturalWidth", "naturalHeight"];
    if (side !== "browser" || !text(value.selector) || !reads.includes(value.read)
      || value.read === "attribute" && !text(value.attribute) || value.read === "css" && !text(value.property)) throw new Error("Invalid browser DOM observation");
    if (["sameRow", "rowGap", "columnGap", "below", "sameRight", "sameColumn"].includes(String(value.read)) && !text(value.relativeTo)) throw new Error("Relative DOM geometry requires an actual reference selector");
    if (value.relativeTo !== undefined && !text(value.relativeTo)) throw new Error("Invalid DOM geometry reference selector");
    if (value.unit !== undefined && !["pixels", "cells", "fraction"].includes(value.unit)) throw new Error("Invalid DOM geometry unit");
    if (value.pattern !== undefined) {
      if (!text(value.pattern) || value.flags !== undefined && typeof value.flags !== "string"
        || value.group !== undefined && (!Number.isInteger(value.group) || value.group < 0)) throw new Error("Invalid browser DOM pattern");
      new RegExp(value.pattern, value.flags);
    } else if (value.flags !== undefined || value.group !== undefined) throw new Error("DOM pattern flags/group require a pattern");
  } else if (value.type === "effect") {
    if (!text(value.channel) || value.occurrence !== undefined && (!Number.isInteger(value.occurrence) || value.occurrence < 0)) throw new Error("Invalid actual effect observation");
  } else if (value.type === "file") {
    if (!text(value.path)) throw new Error("Invalid actual file observation");
  } else throw new Error(`Unsupported actual observation ${observationType}`);
}

function meaningfulValue(value: unknown): boolean {
  if (value === null || value === undefined) return false;
  if (typeof value === "string") return value.trim().length > 0;
  if (Array.isArray(value)) return value.length > 0;
  if (record(value)) return Object.keys(value).length > 0;
  return typeof value === "boolean" || typeof value === "number" && Number.isFinite(value);
}

export function positiveRendering(comparison: ExecutedComparison): boolean {
  return comparison.expectedByAdapter === undefined && comparison.terminal.type === "screen"
    && comparison.browser.type === "dom" && record(comparison.terminalMatch)
    && typeof comparison.terminalMatch.text === "string" && comparison.terminalMatch.text.trim().length > 0
    && record(comparison.browserTarget) && comparison.browserTarget.visible === true
    && meaningfulValue(comparison.terminalValue) && meaningfulValue(comparison.browserValue)
    && !(comparison.terminal.read === "visible" && comparison.terminalValue !== true)
    && !(comparison.browser.read === "visible" && comparison.browserValue !== true)
    && !(comparison.browser.read === "count" && (typeof comparison.browserValue !== "number" || comparison.browserValue <= 0));
}

export function equivalent(left: unknown, right: unknown): boolean {
  if (left === undefined || right === undefined) return false;
  if (typeof left === "number" && !Number.isFinite(left) || typeof right === "number" && !Number.isFinite(right)) return false;
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) && Array.isArray(right)) return left.length === right.length && left.every((value, index) => equivalent(value, right[index]));
  if (record(left) && record(right)) {
    const keys = Object.keys(left);
    return keys.length === Object.keys(right).length && keys.every(key => owns(right, key) && equivalent(left[key], right[key]));
  }
  return false;
}

function nativeConformanceReasons(entry: InventoryEntry, proofs: readonly NativeContractProof[]): string[] {
  const reasons = proofs.length ? [] : ["No executed source-bound native-only request/response or canonical input consequence"];
  // A value only the ACP relay writes has no browser-originated frame to conform to; its proof is the encoder audit.
  const relayOwned = relayOwnedValues(entry);
  for (const value of Array.isArray(entry.details.enumValues) ? entry.details.enumValues.filter(item => !relayOwned.some(owned => equivalent(owned, item))) : []) {
    if (!proofs.some(proof => equivalent(proof.value, value))) reasons.push(`No native conformance for source enum value ${JSON.stringify(value)}`);
  }
  if (record(entry.details.enumDomain) && entry.details.enumDomain.unresolved === true) reasons.push("Unresolved source property value domain");
  return reasons;
}

/** Recompute scalar normalizations from independently retained observations, not report values. */
export function validateNormalizedObservation(comparison: ExecutedComparison): void {
  const normalization = comparison.normalize;
  if (!["number", "whitespace", "percent-fraction", "percent-remaining-fraction"].includes(normalization ?? "")) return;
  for (const adapter of ["terminal", "browser"] as const) {
    const key = adapter === "terminal" ? "terminalRawValue" : "browserRawValue";
    if (!owns(comparison, key)) throw new Error(`Normalized ${adapter} observation lacks its actual raw value`);
    const raw = comparison[key];
    let normalized: unknown;
    if (normalization === "whitespace") {
      if (typeof raw !== "string") throw new Error("Whitespace normalization requires an actual string");
      normalized = raw.replace(/\s+/gu, " ").trim();
    } else {
      const number = typeof raw === "number" ? raw : typeof raw === "string" && raw.trim() ? Number(raw) : NaN;
      if (!Number.isFinite(number)) throw new Error("Numeric normalization requires an actual finite raw value");
      normalized = normalization === "percent-fraction" ? number / 100 : normalization === "percent-remaining-fraction" ? 1 - number / 100 : number;
    }
    if (!equivalent(normalized, comparison[adapter === "terminal" ? "terminalValue" : "browserValue"])) throw new Error(`Normalized ${adapter} observation differs from its actual raw value`);
  }
}

import { createHash } from "node:crypto";
import type { Comparison, Observation } from "./scenario";
import type { InventoryEntry } from "./inventory-types";
import type { WireObservation } from "./probe";
import { equivalent, validateObservation, type BrowserTarget, type ScenarioEvidence } from "./coverage";
import { WireSchemaEvidence } from "./coverage-schema";
import { pointerValue } from "./observation-pointer";

export type NativeObservation = Exclude<Observation, { type: "screen" | "ansi" }>;
export interface WireReference {
  verb: string; direction: "out" | "in"; occurrence: number | "last"; pointer?: string;
  match?: Record<string, unknown>; target?: { selector: string };
}
export interface NativeAssertion { name: string; browser: NativeObservation; expected: unknown; normalize?: Comparison["normalize"] }
export interface NativeContract {
  entry: string;
  witness?: "request" | "response";
  request: WireReference;
  value?: unknown;
  response?: WireReference;
  correlate?: Array<{ request: string; response: string }>;
  assertions: NativeAssertion[];
}
export interface ExecutedNativeAssertion extends NativeAssertion { actual: unknown; passed: boolean; browserTarget?: BrowserTarget }
export interface ExecutedNativeContract extends Omit<NativeContract, "assertions" | "request" | "response"> {
  request: WireReference & { browserTarget?: BrowserTarget }; response?: WireReference & { browserTarget?: BrowserTarget };
  assertions: ExecutedNativeAssertion[];
}
export interface NativeContractProof { entry: string; value: unknown; scenario: string; checkpoint: string; request: { pid: number; generation: string; sequence: number }; response?: { pid: number; generation: string; sequence: number }; assertions: string[] }
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);


/** A single native leg can establish conformance, never a fabricated terminal analogue. */
export function nativeContractProofs(entries: ReadonlyMap<string, InventoryEntry>, scenario: ScenarioEvidence, reader: WireSchemaEvidence): NativeContractProof[] {
  const proofs: NativeContractProof[] = [];
  for (const checkpoint of scenario.checkpoints) for (const contract of checkpoint.nativeContracts ?? []) {
    const entry = entries.get(contract.entry);
    if (!entry || entry.details.exposure !== "native-contract") throw new Error(`Native contract ${contract.entry} lacks source-backed native-only exposure`);
    if (scenario.covers.includes(entry.id)) throw new Error("Native-only conformance cannot also claim terminal UI parity");
    const cursor = checkpoint.probeCursors?.browser;
    if (!cursor) throw new Error("Native conformance requires actual browser source provenance");
    const select = (reference: WireReference & { browserTarget?: BrowserTarget }): WireObservation => {
      if (!record(reference) || reference.occurrence !== "last" && (!Number.isInteger(reference.occurrence) || Number(reference.occurrence) < 0)
        || typeof reference.verb !== "string" || !["in", "out"].includes(reference.direction)) throw new Error("Malformed native wire reference");
      const rows = scenario.wire.filter(row => row.adapter === "browser" && row.origin !== "relay" && row.pid === cursor.pid && row.generation === cursor.generation
        && row.sequence <= cursor.sequence && row.verb === reference.verb && row.direction === reference.direction
        && Object.entries(reference.match ?? {}).every(([pointer, value]) => equivalent(pointerValue(row.body, pointer), value))
        && (!reference.target || reference.browserTarget?.node && reference.browserTarget.surface && record(row.body)
          && row.body.sf === reference.browserTarget.surface && (row.verb !== "e" || row.body.id === reference.browserTarget.node)));
      const row = reference.occurrence === "last" ? rows.at(-1) : rows[Number(reference.occurrence)];
      if (!row) throw new Error(`Native contract ${entry.id} has no actual correlated request/response`);
      return row;
    };
    const request = select(contract.request);
    const response = contract.response ? select(contract.response) : undefined;
    if (contract.witness !== undefined && !["request", "response"].includes(contract.witness)) throw new Error("Invalid native witness side");
    const witness = contract.witness === "response" ? response : request;
    const reference = contract.witness === "response" ? contract.response : contract.request;
    if (!witness || !reference) throw new Error("Native response witness requires its actual response");
    const inspected = reader.inspect(witness);
    if (!inspected.facts.has(entry.id)) throw new Error(`Native contract ${entry.id} is absent from its actual typed source witness`);
    // Alternative schema branches can each claim the same wire location; one pointer is one field.
    const fields = [...new Map(inspected.values.filter(item => item.entry === entry.id && (reference.pointer === undefined || item.pointer === reference.pointer))
      .map(item => [item.pointer, item])).values()];
    if (entry.category === "property" && fields.length !== 1) throw new Error("Native property contract must select one exact typed source field, never an unrelated/ambiguous wire value");
    const value = entry.category === "property" ? fields[0]!.value : pointerValue(witness.body, reference.pointer);
    if (value === undefined || Object.hasOwn(contract, "value") && !equivalent(value, contract.value)) throw new Error("Native request value does not match its explicit source field assertion");
    if (entry.category === "property" && contract.request.target) {
      const target = contract.request.browserTarget!;
      const document = (scenario.documents ?? []).find(row => row.pid === cursor.pid && row.generation === cursor.generation && row.adapter === "browser"
        && row.sequence <= cursor.sequence && row.surface === target.surface && row.surfaceSequence === target.surfaceSequence);
      if (!document || request.verb !== "f" || !record(request.body) || request.body.s !== document.surfaceSequence
        || !reader.document(document).values.some(item => item.entry === entry.id && item.node === target.node && equivalent(item.value, value))) throw new Error("Native property request has no exact current canonical target/frame binding");
    }
    if (response) {
      reader.inspect(response);
      if (request.verb === "b") {
        // A blob transfer is answered by no frame; its proof is the earlier terminal query that asked for exactly these bytes.
        const bytes = typeof request.body === "string" ? Buffer.from(request.body, "base64") : Buffer.alloc(0);
        const ids = record(response.body) && response.body.q === "blobs" && Array.isArray(response.body.ids) ? response.body.ids : [];
        if (response.verb !== "q" || response.direction !== "out" || response.sequence >= request.sequence || !bytes.length
          || !ids.includes(createHash("sha256").update(bytes).digest("hex"))) throw new Error("Native blob transfer does not match the blob id its terminal query requested");
      } else if (request.verb === "q" && response.verb === "b") {
        // The relay answers a blobs query locally with an empty "have"; the observable consequence is the terminal sending each id inline as a b frame.
        const bytes = typeof response.body === "string" ? Buffer.from(response.body, "base64") : Buffer.alloc(0);
        const ids = record(request.body) && request.body.q === "blobs" && Array.isArray(request.body.ids) ? request.body.ids : [];
        if (request.direction !== "out" || response.direction !== "out" || response.sequence <= request.sequence || !bytes.length
          || !ids.includes(createHash("sha256").update(bytes).digest("hex"))) throw new Error("Native blob query is not followed by an inline transfer of a requested id");
      } else {
      if (response.sequence <= request.sequence) throw new Error("Native response precedes its actual request");
      if (!Array.isArray(contract.correlate) || !contract.correlate.length) throw new Error("Native request/response requires explicit source correlation fields");
      for (const pair of contract.correlate) if (!record(pair) || typeof pair.request !== "string" || typeof pair.response !== "string"
        || !equivalent(pointerValue(request.body, pair.request), pointerValue(response.body, pair.response))) throw new Error("Native response correlation does not match actual wire values");
      }
    } else if (request.direction === "out" && ["t", "x"].includes(request.verb)
      || request.direction === "in" && request.verb === "r" && record(request.body) && request.body.r === "hello") {
      // Palette and close are unsolicited pushes: no request exists to correlate, so the
      // browser consequences below are the only proof the push was applied.
    } else if (!(entry.category === "property" && request.verb === "f" && contract.request.target)) {
      if (request.direction !== "in" || request.verb !== "e" || !record(request.body)
        || !(scenario.raw ?? []).some(row => row.type === "native-event" && row.pid === request.pid && row.generation === request.generation
          && typeof row.sequence === "number" && row.sequence > request.sequence && row.sequence <= cursor.sequence && equivalent(row.event, request.body))) throw new Error("Native input requires its actual canonical dispatcher completion, not wire presence alone");
    }
    if (!Array.isArray(contract.assertions) || !contract.assertions.length) throw new Error("Native conformance needs actual browser/controller/host consequences");
    const names = new Set<string>();
    for (const assertion of contract.assertions) {
      validateObservation(assertion.browser, "browser");
      if (!record(assertion) || typeof assertion.name !== "string" || !assertion.name || names.has(assertion.name) || assertion.passed !== true
        || !record(assertion.browser) || !["dom", "effect", "file"].includes(assertion.browser.type)
        || !Object.hasOwn(assertion, "actual") || !Object.hasOwn(assertion, "expected") || !equivalent(assertion.actual, assertion.expected)) throw new Error("Native conformance outcome is missing, malformed or differs from its actual observation");
      names.add(assertion.name);
      if (assertion.browser.type === "dom" && assertion.browser.read !== "count" && !assertion.browserTarget) throw new Error("Native DOM consequence requires its actual matched element provenance");
    }
    const origin = (row: WireObservation) => ({ pid: row.pid, generation: row.generation!, sequence: row.sequence });
    proofs.push({ entry: entry.id, value, scenario: scenario.id, checkpoint: checkpoint.name, request: origin(request), ...(response ? { response: origin(response) } : {}), assertions: [...names] });
  }
  return proofs;
}

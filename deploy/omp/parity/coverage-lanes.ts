import { relayProofHolds } from "./coverage-relay";
import type { InventoryEntry } from "./inventory-types";

export type CoverageLane = "tui-html-parity" | "native-contract-conformance" | "source-audit";
export interface SourceAudit { entry: string; reason: "config-only-setting" | "adapter-hidden-setting" | "non-component-render-helper" | "unproduced-wire-member"; proof: Record<string, unknown> }
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const location = (value: unknown) => record(value) && typeof value.source === "string" && value.source.startsWith("packages/") && Number.isInteger(value.line) && Number(value.line) > 0;

/** Classifies only source-proven non-UI contracts; unresolved exposure retains its parity obligation. */
export function sourceAudit(entry: InventoryEntry): SourceAudit | undefined {
  const details = entry.details;
  if (entry.category === "setting" && details.exposure === "config-only") {
    const proof = details.presentationProof;
    if (details.configOnly !== true || !record(proof) || !record(proof.ui) || proof.ui.value !== "undefined" || !location(proof.ui.getter)
      || !location(details.declaration)) throw new Error(`Config-only ${entry.id} lacks its actual declaration/getter source proof`);
    return { entry: entry.id, reason: "config-only-setting", proof };
  }
  if (entry.category === "setting" && details.exposure === "ui-hidden") {
    const proof = details.presentationProof;
    if (details.configOnly !== false || !record(proof) || !record(proof.display) || proof.display.value !== "null" || !location(proof.display.adapter)
      || !location(details.declaration)) throw new Error(`Hidden ${entry.id} lacks its display-adapter source proof`);
    return { entry: entry.id, reason: "adapter-hidden-setting", proof };
  }
  if (entry.category === "render-helper" && details.exposure === "non-component-helper") {
    const proof = details.classification;
    if (!record(proof) || !record(proof.componentContract) || proof.componentContract.declared !== false || proof.componentContract.structural !== false
      || !location(proof.declaration) || !Array.isArray(proof.terminalReturns) || !proof.terminalReturns.length
      || !proof.terminalReturns.every(value => record(value) && ["string", "value"].includes(String(value.shape)))) throw new Error(`Render helper ${entry.id} lacks its actual non-Component return-shape source proof`);
    return { entry: entry.id, reason: "non-component-render-helper", proof };
  }
  if (["property", "operation", "event", "kind", "reply"].includes(entry.category) && details.exposure === "unproduced") {
    const proof = details.unproducedProof;
    const named = entry.category === "property" ? entry.id.endsWith(`.${String(record(proof) ? proof.key : "")}`) : entry.id === `${entry.category}:${String(record(proof) ? proof.key : "")}`;
    if (!record(proof) || typeof proof.key !== "string" || !named || !Array.isArray(proof.declarations) || !proof.declarations.length
      || !Number.isSafeInteger(proof.searchedFiles) || Number(proof.searchedFiles) <= 0 || (Array.isArray(details.uiEmitters) && details.uiEmitters.length)
      || proof.proof === "parent-never-written" && typeof proof.parent !== "string"
      || (entry.category === "kind" || proof.proof === "producer-unreferenced") && !unreferenced(proof.producers)
      || (entry.category === "reply" || proof.proof === "relay-owned-reply") && !(entry.category === "reply" && proof.proof === "relay-owned-reply" && relayProofHolds(proof.relay, proof.key)))
      throw new Error(`Unproduced ${entry.id} lacks its key-write search proof`);
    return { entry: entry.id, reason: "unproduced-wire-member", proof };
  }
  return undefined;
}

/** Each producer is located inside a located function whose name occurs once: its own declaration. */
function unreferenced(producers: unknown): boolean {
  return Array.isArray(producers) && producers.length > 0 && producers.every(item => record(item) && location(item.producer)
    && location(item.function) && record(item.function) && typeof item.function.name === "string" && item.occurrences === 1);
}

export function coverageLane(entry: InventoryEntry): CoverageLane {
  if (sourceAudit(entry)) return "source-audit";
  return entry.details.exposure === "native-contract" ? "native-contract-conformance" : "tui-html-parity";
}

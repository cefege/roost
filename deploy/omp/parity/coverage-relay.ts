//! Validates the proof that a reply is written only by the ACP relay. Read by
//! `coverage-lanes.ts` (the reply entry) and `coverage.ts` (the shared
//! discriminator's value); produced by `inventory-relay-owned.ts`.
import type { InventoryEntry } from "./inventory-types";

export const ACP_RELAY = "packages/coding-agent/src/modes/acp/";
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);

/** Every encoder call that can write a reply frame is a located call in the relay that spells this tag. */
export function relayProofHolds(relay: unknown, tag: string): boolean {
  return record(relay) && relay.tag === tag && Array.isArray(relay.writes) && relay.writes.length > 0 && Number.isInteger(relay.calls) && Number(relay.calls) >= relay.writes.length
    && relay.writes.every(write => record(write) && write.tag === tag && record(write.at) && typeof write.at.source === "string" && write.at.source.startsWith(ACP_RELAY)
      && Number.isInteger(write.at.line) && Number(write.at.line) > 0)
    && record(relay.verbDeclaration) && typeof relay.verbDeclaration.source === "string" && Number.isInteger(relay.verbDeclaration.line);
}

/** Discriminator values only the relay writes, each backed by its proof. */
export function relayOwnedValues(entry: InventoryEntry): unknown[] {
  const owned = entry.details.relayOwnedValues;
  if (!record(owned) || owned.proof !== "relay-owned-reply" || !Array.isArray(owned.values) || owned.values.length !== 1 || typeof owned.values[0] !== "string" || typeof owned.reply !== "string") return [];
  return owned.reply === `reply:${owned.values[0]}` && relayProofHolds(owned.relay, owned.values[0]) ? owned.values : [];
}

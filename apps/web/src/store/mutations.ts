// Store-layer optimistic writes. Components mutate the root store ONLY through
// these named functions, never via inline setRootStore — ARCHITECTURE.md:42
// ("components never mutate the store directly"). Each write mirrors a successful
// coordClient RPC so the reactive UI reflects the change before the Sync delta
// lands (same optimistic pattern as store/optimisticSpawn.ts).
// Callers: Onboarding.tsx, PairRequestNotifier.tsx (pair_requests);
// Settings/McpPane.tsx (mcp_relays).

import type { McpRelay } from "@roost/shared/wire";
import { deleteStoreRecord, setRootStore } from "./root.ts";

export function deletePairRequest(ephemeralId: string): void {
  // Per-key delete; setRootStore(key, fn → newRecord) silently no-ops on a
  // Record subtree (feedback_solid_setstore_record_replace).
  deleteStoreRecord("pair_requests", ephemeralId);
}

export function replaceMcpRelays(record: Record<string, McpRelay>): void {
  setRootStore("mcp_relays", record);
}

export function upsertMcpRelay(relay: McpRelay): void {
  setRootStore("mcp_relays", relay.id, relay);
}

export function deleteMcpRelay(id: string): void {
  // Per-key delete; setRootStore(key, fn → newRecord) silently no-ops on a
  // Record subtree (feedback_solid_setstore_record_replace).
  deleteStoreRecord("mcp_relays", id);
}

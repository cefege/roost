// Shared endpoint and process constants for the multiplexed keeper pool.
// Internal-only — not part of the public import surface.

import { fileURLToPath } from "node:url";
import { resolveLocalEndpoint, type LocalEndpoint } from "@roost/shared/local-endpoint";
import { workerDataDir } from "@roost/shared/paths";

export const MUX_KEEPER_ENDPOINT_NAME = "mux-keeper";

let resolvedMuxEndpoint: LocalEndpoint | null = null;

/** Resolve once per process so every pool operation uses the same persisted
 * capability and (on Windows) randomized user-scoped named-pipe address. */
export function muxLocalEndpoint(): LocalEndpoint {
  if (!resolvedMuxEndpoint) {
    resolvedMuxEndpoint = resolveLocalEndpoint({
      name: MUX_KEEPER_ENDPOINT_NAME,
      dataDir: workerDataDir(),
    });
  }
  return resolvedMuxEndpoint;
}

// Bun runs .ts directly (no transpile step). multiplexed-main.ts is the
// keeper entry — same source the worker imports types from, no build
// step, no parallel .js artifact. fileURLToPath preserves Windows drive
// letters; URL.pathname would produce an unusable /C:/... command argument.
export const MUX_KEEPER_MAIN_TS = fileURLToPath(new URL("./multiplexed-main.ts", import.meta.url));

// Always use the same Bun binary the worker is currently running on —
// guaranteed present, no path search needed. (Was previously findNode()
// when the keeper had to be Node for node-pty compat; Bun 1.3 ships
// native PTY via Bun.spawn({terminal: {...}}) so the runtime split is
// gone — see multiplexed-main.ts header.)
export const BUN_BIN = process.execPath;

// A healthy keeper acks a Spawn frame in <100ms. 8s = generous slack for a
// loaded box before we declare the keeper wedged (spawn.no_ack signal).
export const SPAWN_ACK_TIMEOUT_MS = 8000;

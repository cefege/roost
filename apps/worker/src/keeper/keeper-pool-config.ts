// Shared module constants + socket-path helper for the multiplexed keeper
// pool. Extracted from multiplexed-client.ts so the pool's split-out method
// modules (keeper-pool-*.ts) can import them without cycling back through the
// client. Internal-only — not part of the public import surface.

import { join } from "node:path";
import { workerDataDir } from "@roost/shared";

// Bun runs .ts directly (no transpile step). multiplexed-main.ts is the
// keeper entry — same source the worker imports types from, no build
// step, no parallel .js artifact.
export const MUX_KEEPER_MAIN_TS = new URL("./multiplexed-main.ts", import.meta.url).pathname;

export function muxSocketPath(): string {
  return join(workerDataDir(), "mux-keeper.sock");
}

// Always use the same Bun binary the worker is currently running on —
// guaranteed present, no path search needed. (Was previously findNode()
// when the keeper had to be Node for node-pty compat; Bun 1.3 ships
// native PTY via Bun.spawn({terminal: {...}}) so the runtime split is
// gone — see multiplexed-main.ts header.)
export const BUN_BIN = process.execPath;

// A healthy keeper acks a Spawn frame in <100ms. 8s = generous slack for a
// loaded box before we declare the keeper wedged (spawn.no_ack signal).
export const SPAWN_ACK_TIMEOUT_MS = 8000;

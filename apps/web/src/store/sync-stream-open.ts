// Firehose WebSocket (coord-sync) liveness signal — leaf module to avoid import cycles.
// sync.ts writes, CellTerminal reads. No other imports so the signal stays clean.

import { createSignal } from "solid-js";

const [_open, setOpen] = createSignal(false);

export { setOpen };

/** True while the firehose WebSocket (/ws/coord-sync) is OPEN. Reactive —
 *  CellTerminal re-claims on the false→true rising edge so a reconnect always
 *  re-delivers the tail cell frame onto a live socket. */
export function syncStreamOpen(): boolean { return _open(); }

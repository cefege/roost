// SessionEventSink — single emit boundary for SessionEvents leaving the
// worker process. Callers (`session-manager.ts`, hook listener in
// `main.ts`, `snapshot.ts`) don't know which transport carries the
// bytes; phase-25d retired the migration-era teeSink + trpcSink so
// there's now only one option: CoordLink.

import type { SessionEvent } from "@roost/shared/wire";
import type { CoordLink } from "./transport/CoordLink.ts";

export interface SessionEventSink {
  emit(event: SessionEvent): void;
}

/** Sink that pushes events as `event` frames on the outbound CoordLink.
 * Pre-open events queue inside CoordLink (FIFO, drop-oldest at 1024). */
export function coordLinkSink(link: CoordLink): SessionEventSink {
  return {
    emit(event: SessionEvent): void {
      link.send({ kind: "event", event });
    },
  };
}

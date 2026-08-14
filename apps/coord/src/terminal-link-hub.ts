// terminal-link-hub — parse coordinator-internal PTY bytes once and publish
// only completed, sanitized OSC 8 text-to-URI mappings to browser Sync feeds.

import { Osc8Tracker } from "@roost/shared/terminal-osc8";
import { globalBytesBus, sessionBus, terminalLinkBus } from "./buses.ts";

interface ActiveHub {
  refs: number;
  parsers: Map<string, Osc8Tracker>;
  unsubscribeBytes: () => void;
  unsubscribeSessions: () => void;
}

let activeHub: ActiveHub | null = null;

/** Start the process-wide terminal-link parser hub. Callers share one pair of
 * bus subscriptions; the last disposer clears all parser state. */
export function startTerminalLinkHub(): () => void {
  if (activeHub) {
    activeHub.refs += 1;
    return makeDisposer(activeHub);
  }

  const parsers = new Map<string, Osc8Tracker>();
  const unsubscribeBytes = globalBytesBus.subscribe(({ session_id, bytes }) => {
    if (bytes.byteLength === 0) return;
    let parser = parsers.get(session_id);
    if (!parser) {
      parser = new Osc8Tracker((text, uri) => {
        terminalLinkBus.publish({ session_id, text, uri });
      });
      parsers.set(session_id, parser);
    }
    parser.process(bytes);
  });
  const unsubscribeSessions = sessionBus.subscribe((event) => {
    if (event.kind === "closed") parsers.delete(event.session_id);
  });

  const hub: ActiveHub = {
    refs: 1,
    parsers,
    unsubscribeBytes,
    unsubscribeSessions,
  };
  activeHub = hub;
  return makeDisposer(hub);
}

function makeDisposer(hub: ActiveHub): () => void {
  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    if (activeHub !== hub) return;
    hub.refs -= 1;
    if (hub.refs > 0) return;
    hub.unsubscribeBytes();
    hub.unsubscribeSessions();
    hub.parsers.clear();
    activeHub = null;
  };
}

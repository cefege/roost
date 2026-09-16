// A local terminal transport whose socket is not writable: publishView records
// the attempt and returns false, exactly as ws/local-terminal.ts answers for a
// socket that is not OPEN. Store tests use it to drive the publication paths
// that only a refused write reaches; release() hands the session back to Sync
// so the registration cannot outlive one case.

import {
  LOCAL_TERMINAL_PROCESS_EPOCH,
  registerTerminalLocalTransport,
  type TerminalLocalTransport,
} from "../../src/store/terminal-stream-transport.ts";

export interface RefusingLocalTransport {
  /** One entry per command that reached the socket, refused or not. */
  readonly publishedViewIds: string[];
  writeAccepted: boolean;
  release(): void;
}

export function installRefusingLocalTransport(
  sessionId: string,
): RefusingLocalTransport {
  const publishedViewIds: string[] = [];
  let ownedSessionId: string | null = sessionId;
  const control: RefusingLocalTransport = {
    publishedViewIds,
    writeAccepted: false,
    release: () => { ownedSessionId = null; },
  };
  const transport: TerminalLocalTransport = {
    ownsSession: (candidate) => candidate === ownedSessionId,
    noteViewPublished: () => {},
    generationToken: () => ({
      socketGeneration: 1,
      socketId: "local-socket-1",
      processEpoch: LOCAL_TERMINAL_PROCESS_EPOCH,
      domainGeneration: 0n,
    }),
    publishView: (command) => {
      publishedViewIds.push(command.viewId);
      return control.writeAccepted;
    },
    publishResync: () => control.writeAccepted,
    sendInput: () => ({ outcome: "rejected", reason: "test" }) as never,
    requestScrollback: () => Promise.reject(new Error("unused")),
    redial: () => true,
    reset: () => {},
  };
  registerTerminalLocalTransport(transport);
  return control;
}

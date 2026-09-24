// A registered direct connection whose view writes can be refused deterministically.
// Publication tests first promote it through the real registry, then exercise retry scheduling.
// It never aliases the retired local-transport singleton or bypasses route election.
// release unregisters this exact connection so one test cannot retain another's route.

import type { TerminalViewCommand } from "@roost/protocol/proto/sync_pb";
import {
  terminalDirectRegistry,
  type TerminalDirectConnection,
} from "../../src/store/terminal-stream-transport.ts";
import type { TerminalGenerationToken } from "../../src/store/terminal-stream-types.ts";

export interface RefusingDirectTransport {
  readonly connection: TerminalDirectConnection;
  readonly publishedViewIds: string[];
  readonly publishedViewCommands: TerminalViewCommand[];
  readonly token: TerminalGenerationToken;
  writeAccepted: boolean;
  release(): void;
}

export function installRefusingDirectTransport(
  sessionId: string,
  workerFp: string,
): RefusingDirectTransport {
  const publishedViewIds: string[] = [];
  const publishedViewCommands: TerminalViewCommand[] = [];
  const token: TerminalGenerationToken = {
    socketGeneration: 41,
    socketId: "refusing-direct-socket",
    processEpoch: "refusing-direct-worker",
    domainGeneration: 0n,
    transportKind: "loopback",
    workerFp,
  };
  let writeAccepted = true;
  const connection: TerminalDirectConnection = {
    workerFp,
    kind: "loopback",
    connectionId: "refusing-direct-connection",
    workerEpoch: token.processEpoch,
    inputRouteSupported: false,
    token: () => token,
    allowsSession: (candidate) => candidate === sessionId,
    publishView: (command: TerminalViewCommand) => {
      publishedViewIds.push(command.viewId);
      publishedViewCommands.push(command);
      return writeAccepted;
    },
    publishResync: () => writeAccepted,
    sendInput: () => "refused",
    claimInputRoute: async () => { throw new Error("unused direct test claim"); },
    requestScrollback: async () => { throw new Error("unused direct test scrollback"); },
    probe: async () => undefined,
    close: () => undefined,
  };
  const unregister = terminalDirectRegistry.register(connection);
  return {
    connection,
    publishedViewIds,
    publishedViewCommands,
    token,
    get writeAccepted() { return writeAccepted; },
    set writeAccepted(next) { writeAccepted = next; },
    release: unregister,
  };
}

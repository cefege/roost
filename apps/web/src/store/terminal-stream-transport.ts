// The registration seam for the local terminal transport. A session is
// published over exactly ONE transport: the local worker socket when that
// socket holds a grant for it, otherwise Sync. ws/local-terminal.ts registers
// the implementation here and terminal-stream-publication.ts turns the answer
// into a publication target, so nothing in the store reaches into ws/ — this
// module deliberately has no runtime dependencies at all.

import type { LocalScrollbackResponse } from "@roost/shared/proto/local_terminal_pb";
import type {
  TerminalResyncCommand,
  TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import type { InputAdmission } from "../ws/terminal-input-lanes.ts";
import type { TerminalGenerationToken } from "./terminal-stream-types.ts";

/** Process epoch of every local-socket generation token. It can never collide
 * with a coordinator-minted Sync epoch, so a frame from one transport can never
 * satisfy the other's generation fence. */
export const LOCAL_TERMINAL_PROCESS_EPOCH = "local-terminal";

export interface LocalScrollbackQuery {
  sessionId: string;
  endRow: bigint;
  maxRows: number;
  gridEpoch: string;
}

export interface TerminalLocalTransport {
  /** Granted by the coordinator and acknowledged by a ready local socket. */
  ownsSession(sessionId: string): boolean;
  /** A pane published a view for this session; the local transport decides
   * whether the session is eligible for a grant. */
  noteViewPublished(sessionId: string): void;
  generationToken(): TerminalGenerationToken | null;
  publishView(command: TerminalViewCommand): boolean;
  publishResync(command: TerminalResyncCommand): boolean;
  sendInput(sessionId: string, bytes: Uint8Array, viewId?: string): InputAdmission;
  /** The local mirror of SessionsGetScrollbackCells, so history paging follows
   * the same transport as the live grid. */
  requestScrollback(query: LocalScrollbackQuery): Promise<LocalScrollbackResponse>;
  /** Last-resort recovery for a stalled local pane: replace the socket. */
  redial(reason: string): boolean;
  reset(reason: string): void;
}

let transport: TerminalLocalTransport | null = null;
const transportChangeHandlers = new Set<() => void>();

export function registerTerminalLocalTransport(next: TerminalLocalTransport): void {
  transport = next;
}

export function localTerminalTransport(): TerminalLocalTransport | null {
  return transport;
}

/** ws/local-terminal.ts calls this whenever its grant set or socket generation
 * changes. The store then retargets those sessions, which mints a fresh stream
 * on the new transport and delivers a complete baseline before any delta. */
export function notifyTerminalLocalTransportChanged(): void {
  for (const handler of transportChangeHandlers) handler();
}

export function registerTerminalLocalTransportHandler(handler: () => void): void {
  transportChangeHandlers.add(handler);
}

export function isLocalTerminalGenerationToken(
  token: TerminalGenerationToken | null,
): boolean {
  return token?.processEpoch === LOCAL_TERMINAL_PROCESS_EPOCH;
}

/** The local socket's generation for this session, or null when Sync owns it. */
export function localTerminalGenerationToken(
  sessionId: string,
): TerminalGenerationToken | null {
  if (!transport?.ownsSession(sessionId)) return null;
  return transport.generationToken();
}

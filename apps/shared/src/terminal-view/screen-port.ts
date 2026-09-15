// The screen delivery surface a terminal view host must provide.
// TerminalViewRegistry calls exactly these six methods when membership,
// watch state, or a client checkpoint changes; the host decides how a
// baseline or delta reaches the socket. Coord's TerminalScreenHub and the
// worker's local cell delivery both satisfy this structurally, so neither
// host is wrapped in an adapter or cast at the construction site.

export interface TerminalViewScreenPort {
  /** `sink` is the host's own socket object, handed straight back on every
   * later call for that socketId. The registry never reads it, so its shape
   * stays private to the host. */
  registerSocket(socketId: string, sink: unknown): void;
  unregisterSocket(socketId: string): void;
  setWatching(socketId: string, sessionId: string, watching: boolean): void;
  /** True when a baseline for the session's current stream was queued. */
  seedSocket(socketId: string, sessionId: string): boolean;
  /** True when the socket entered a stream it was not already painting, so
   * the caller owes it a baseline. */
  ensureSocketStream(socketId: string, sessionId: string): boolean;
  /** Serve the socket forward from its own checkpoint, or force a fresh
   * baseline when the host cannot prove the checkpoint is still reachable. */
  resyncSocket(
    socketId: string,
    sessionId: string,
    at: { gridEpoch: string; seq: bigint },
  ): void;
}

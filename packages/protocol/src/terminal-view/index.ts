// @roost/protocol/terminal-view — the one implementation of terminal view
// membership: which socket watches which session, lease renewal, park and
// sweep, and the per-viewer geometry hosts minimize over. Coord's view hub and
// the worker's view owner each construct TerminalViewRegistry with their own
// TerminalViewScreenPort. Server-side only (it logs through ./log.ts).
// Import via "@roost/protocol/terminal-view".

export * from "./screen-port.ts";
export * from "./terminal-view-protocol.ts";
export * from "./terminal-view-registry.ts";
export * from "./terminal-view-registry-commands.ts";
export * from "./terminal-view-registry-operations.ts";
export * from "./terminal-view-registry-state.ts";

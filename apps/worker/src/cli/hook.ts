// Hook subcommand: invoked by claude --settings hooks.
// Reads hook payload from stdin (JSON), adds event kind from the event arg,
// POSTs one JSON line to the worker's hook UDS at $ROOST_HOOK_SOCKET.
// Exits 0 immediately after write so claude is not blocked.
//
// Entry for BOTH `bun run cli/hook.ts <event>` (from source) and
// `roost hook <event>` (compiled binary, via roost-cli/hook.ts). The body lives
// in runHook() so importing for the subcommand has no side effects.

import { createConnection } from "node:net";

export function runHook(event: string): void {
  const socketPath = process.env.ROOST_HOOK_SOCKET ?? "";
  const agent = process.env.ROOST_SURFACE_ID ?? "";

  if (!socketPath) process.exit(0);

  let stdinData = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk: string) => { stdinData += chunk; });
  process.stdin.on("end", () => {
    let payload: unknown;
    try { payload = JSON.parse(stdinData); } catch { payload = {}; }

    const line = JSON.stringify({ event, agent, payload }) + "\n";

    const sock = createConnection(socketPath, () => {
      sock.write(line, () => { sock.destroy(); process.exit(0); });
    });
    sock.on("error", () => process.exit(0));
    // Safety timeout — never block claude.
    setTimeout(() => process.exit(0), 2000).unref();
  });
}

if (import.meta.main) {
  runHook(process.argv[2] ?? "unknown");
}

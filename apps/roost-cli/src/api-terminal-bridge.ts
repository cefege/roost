// Stable machine-readable session discovery and exact-byte terminal input.
// api.ts delegates the sessions and input verbs here after authentication.
// The injected I/O boundary keeps stdin byte handling and RPC cardinality testable.
// SessionsInput remains one-shot: transport errors propagate without retries.

import type { CoordClient } from "../../worker/src/coord-client.ts";

export const TERMINAL_BRIDGE_MAX_INPUT_BYTES = 65_536;

export interface TerminalBridgeIo {
  readStdin(): Promise<Uint8Array>;
  writeLine(line: string): void;
  writeExitCode(code: number): void;
}

type TerminalBridgeClient = Pick<CoordClient, "sessionsInput" | "sessionsList">;

const defaultTerminalBridgeIo: TerminalBridgeIo = {
  async readStdin() {
    return new Uint8Array(await Bun.stdin.arrayBuffer());
  },
  writeLine(line) {
    console.log(line);
  },
  writeExitCode(code) {
    process.exitCode = code;
  },
};

export async function dispatchTerminalBridgeApi(
  client: TerminalBridgeClient,
  verb: string,
  args: readonly string[],
  io: TerminalBridgeIo = defaultTerminalBridgeIo,
): Promise<boolean> {
  if (verb === "sessions") {
    await printSessions(client, args, io);
    return true;
  }
  if (verb === "input") {
    await submitTerminalInput(client, args, io);
    return true;
  }
  return false;
}

async function printSessions(
  client: TerminalBridgeClient,
  args: readonly string[],
  io: TerminalBridgeIo,
): Promise<void> {
  if (args.length > 1 || (args[0] !== undefined && args[0] !== "--json")) {
    throw new Error("sessions: expected [--json]");
  }
  const { sessions } = await client.sessionsList({ status: "all" });
  if (args[0] === "--json") {
    io.writeLine(JSON.stringify(sessions.map((session) => ({
      id: session.id,
      workerFp: session.workerFp,
      cwd: session.cwd,
      spawnCwd: session.spawnCwd ?? "",
      title: session.customTitle || "",
      status: session.status,
    }))));
    return;
  }
  for (const session of sessions) {
    io.writeLine([session.id, session.workerFp, session.kind, session.cwd, session.customTitle || ""].join("\t"));
  }
}

async function submitTerminalInput(
  client: TerminalBridgeClient,
  args: readonly string[],
  io: TerminalBridgeIo,
): Promise<void> {
  if (args.includes("--help")) {
    if (args.length !== 1) throw new Error("input: --help cannot be combined with other arguments");
    io.writeLine("Usage: roost api input <sessionId> (<escaped-text> | --stdin) [--enter]");
    return;
  }
  const parsed = parseInputArgs(args);
  const sourceBytes = parsed.stdin
    ? await io.readStdin()
    : new TextEncoder().encode(expandEscapedText(parsed.positionalText!));
  const data = appendEnter(sourceBytes, parsed.enter);
  const response = await client.sessionsInput({ sessionId: parsed.sessionId, data });
  if (response.accepted) {
    io.writeLine('{"ok":true,"accepted":true}');
    return;
  }
  io.writeLine('{"ok":false,"accepted":false,"error":"terminal input was not accepted"}');
  io.writeExitCode(2);
}

function parseInputArgs(args: readonly string[]): {
  sessionId: string;
  positionalText: string | undefined;
  stdin: boolean;
  enter: boolean;
} {
  const sessionId = args[0];
  if (!sessionId || sessionId.startsWith("--")) throw new Error("input: missing <sessionId>");
  let positionalText: string | undefined;
  let stdin = false;
  let enter = false;
  for (const argument of args.slice(1)) {
    if (argument === "--stdin") {
      if (stdin) throw new Error("input: duplicate --stdin");
      stdin = true;
      continue;
    }
    if (argument === "--enter") {
      if (enter) throw new Error("input: duplicate --enter");
      enter = true;
      continue;
    }
    if (argument.startsWith("--")) throw new Error(`input: unknown option ${argument}`);
    if (positionalText !== undefined) throw new Error("input: expected one <text>");
    positionalText = argument;
  }
  if (stdin && positionalText !== undefined) {
    throw new Error("input: --stdin cannot be combined with positional text");
  }
  if (!stdin && positionalText === undefined) throw new Error("input: missing <text> or --stdin");
  return { sessionId, positionalText, stdin, enter };
}

function expandEscapedText(text: string): string {
  return text.replace(/\\n/g, "\n").replace(/\\t/g, "\t").replace(/\\r/g, "\r");
}

function appendEnter(source: Uint8Array, enter: boolean): Uint8Array {
  const maximumSourceBytes = enter
    ? TERMINAL_BRIDGE_MAX_INPUT_BYTES - 1
    : TERMINAL_BRIDGE_MAX_INPUT_BYTES;
  if (source.byteLength > maximumSourceBytes) {
    throw new Error(`input: exceeds ${maximumSourceBytes} byte limit${enter ? " before --enter" : ""}`);
  }
  if (!enter) return source;
  const withEnter = new Uint8Array(source.byteLength + 1);
  withEnter.set(source);
  withEnter[source.byteLength] = 0x0d;
  return withEnter;
}

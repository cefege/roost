#!/usr/bin/env bun

import { once } from "node:events";
import { WriteStream } from "node:tty";
import { basename } from "node:path";
import {
  PTY_FIXTURE_PROTOCOL,
  PTY_FIXTURE_READY,
} from "./pty-fixture-protocol.ts";
import type { PtyFixtureCommand } from "./pty-fixture-protocol.ts";

const MAX_FRAME_BYTES = 1024 * 1024;
const MAX_FLOOD_LINES = 100_000;
const MAX_ARMED_LINE_BYTES = 64 * 1024;
const MAX_BUFFERED_INPUT_BYTES = MAX_FRAME_BYTES + 64 * 1024;
const MAX_PENDING_WORK = 32;
const protocolBytes = Buffer.from(PTY_FIXTURE_PROTOCOL, "ascii");
const newlineByte = 0x0a;
const treeChildren: Array<{ pid: number; kill(signal?: number | NodeJS.Signals): void }> = [];
let restored = false;
type ArmedInput =
  | { kind: "legacy-key"; nonce: string }
  | { kind: "cursor-move"; nonce: string }
  | { kind: "line-overwrite"; nonce: string; bytes: number }
  | { kind: "alt-redraw"; nonce: string; trigger: "key" | "line"; bytes: number };

let armedInput: ArmedInput | null = null;
let discardNextLineFeed = false;
let lineOverwriteCounter = 0;
let altRedrawCounter = 0;
let altScreenActive = false;
let bufferedInput = Buffer.alloc(0);
let payloadBytes = -1;
let commandLane = Promise.resolve();
let pendingWork = 0;
let inputFailed = false;

function restoreInput(): void {
  if (restored) return;
  restored = true;
  if (process.stdin.isTTY) process.stdin.setRawMode(false);
}

async function writeOutput(output: string | Uint8Array): Promise<void> {
  if (process.stdout.write(output)) return;
  await once(process.stdout, "drain");
}

function executableCommand(args: string[]): string[] {
  const runtime = basename(process.execPath).toLowerCase();
  return runtime === "bun" || runtime === "bun.exe"
    ? [process.execPath, Bun.main, ...args]
    : [process.execPath, ...args];
}

function spawnTreeChild(depth: number): { pid: number; kill(signal?: number | NodeJS.Signals): void } {
  return Bun.spawn({
    cmd: executableCommand(["--roost-tree-child", String(depth)]),
    stdin: "ignore",
    stdout: "ignore",
    stderr: "ignore",
  });
}

const treeChildArg = process.argv.indexOf("--roost-tree-child");
if (treeChildArg >= 0) {
  const depth = Number.parseInt(process.argv[treeChildArg + 1] ?? "0", 10);
  if (Number.isSafeInteger(depth) && depth > 0) treeChildren.push(spawnTreeChild(depth - 1));
  process.on("SIGINT", () => process.exit(0));
  process.on("SIGTERM", () => process.exit(0));
  setInterval(() => undefined, 60_000);
} else {
  if (process.stdin.isTTY) {
    process.stdin.setRawMode(true);
    process.stdin.resume();
  }

  const requiredString = (value: unknown, field: string): string => {
    if (typeof value !== "string" || value.length === 0 || value.length > 4_096) {
      throw new Error(`${field} must be a non-empty string no longer than 4096 characters`);
    }
    return value;
  };

  const boundedInteger = (value: unknown, field: string, minimum: number, maximum: number): number => {
    if (!Number.isSafeInteger(value) || (value as number) < minimum || (value as number) > maximum) {
      throw new Error(`${field} must be an integer in [${minimum}, ${maximum}]`);
    }
    return value as number;
  };

  const armedInputForCommand = (raw: unknown): ArmedInput | null => {
    if (!raw || typeof raw !== "object" || !("op" in raw) || typeof raw.op !== "string") return null;
    const nonce = "nonce" in raw ? raw.nonce : undefined;
    switch (raw.op) {
      case "ARM_KEY":
        return { kind: "legacy-key", nonce: requiredString(nonce, "nonce") };
      case "ARM_CURSOR_MOVE":
        return { kind: "cursor-move", nonce: requiredString(nonce, "nonce") };
      case "ARM_LINE_OVERWRITE":
        return { kind: "line-overwrite", nonce: requiredString(nonce, "nonce"), bytes: 0 };
      case "ARM_ALT_REDRAW": {
        const trigger = "trigger" in raw ? raw.trigger : undefined;
        if (trigger !== "key" && trigger !== "line") {
          throw new Error("trigger must be key or line");
        }
        return {
          kind: "alt-redraw",
          nonce: requiredString(nonce, "nonce"),
          trigger,
          bytes: 0,
        };
      }
      default:
        return null;
    }
  };

  const nextCounter = (current: number, name: string): number => {
    if (current >= Number.MAX_SAFE_INTEGER) throw new Error(`${name} counter exhausted`);
    return current + 1;
  };

  const handleArmedTrigger = async (armed: ArmedInput): Promise<void> => {
    switch (armed.kind) {
      case "legacy-key":
        await writeOutput(`ACK:${armed.nonce}\r\n`);
        return;
      case "cursor-move":
        await writeOutput("\x1b[1C");
        return;
      case "line-overwrite":
        lineOverwriteCounter = nextCounter(lineOverwriteCounter, "line overwrite");
        await writeOutput(
          `\x1b[1A\r\x1b[2KOVERWRITE:${armed.nonce}:${lineOverwriteCounter}\x1b[1B\r`,
        );
        return;
      case "alt-redraw": {
        altRedrawCounter = nextCounter(altRedrawCounter, "alternate redraw");
        altScreenActive = !altScreenActive;
        const screen = altScreenActive ? "alt" : "main";
        const selectScreen = altScreenActive ? "\x1b[?1049h" : "\x1b[?1049l";
        await writeOutput(
          `${selectScreen}\x1b[H\x1b[2JALT_REDRAW:${armed.nonce}:${altRedrawCounter}:${screen}\r\n`,
        );
        return;
      }
    }
  };

  const handleCommand = async (raw: unknown): Promise<void> => {
    if (!raw || typeof raw !== "object" || !("op" in raw) || typeof raw.op !== "string") {
      throw new Error("command must contain an op string");
    }
    const command = raw as PtyFixtureCommand;
    switch (command.op) {
      case "ARM_KEY": {
        const nonce = requiredString(command.nonce, "nonce");
        await writeOutput(`ARMED:${nonce}\r\n`);
        return;
      }
      case "ARM_CURSOR_MOVE": {
        const nonce = requiredString(command.nonce, "nonce");
        await writeOutput(`ARMED:CURSOR_MOVE:${nonce}\r\n`);
        return;
      }
      case "ARM_LINE_OVERWRITE": {
        const nonce = requiredString(command.nonce, "nonce");
        await writeOutput(`ARMED:LINE_OVERWRITE:${nonce}\r\n`);
        return;
      }
      case "ARM_ALT_REDRAW": {
        const nonce = requiredString(command.nonce, "nonce");
        if (command.trigger !== "key" && command.trigger !== "line") {
          throw new Error("trigger must be key or line");
        }
        await writeOutput(`ARMED:ALT_REDRAW:${nonce}:${command.trigger}\r\n`);
        return;
      }
      case "EMIT": {
        if (command.delayMs !== undefined) {
          await Bun.sleep(boundedInteger(command.delayMs, "delayMs", 0, 30_000));
        }
        if (typeof command.text !== "string" || command.text.length > MAX_FRAME_BYTES) {
          throw new Error("text must be a string no larger than the frame limit");
        }
        await writeOutput(command.newline === false ? command.text : `${command.text}\r\n`);
        return;
      }
      case "FLOOD": {
        const prefix = requiredString(command.prefix, "prefix");
        const count = boundedInteger(command.count, "count", 1, MAX_FLOOD_LINES);
        const start = command.start === undefined
          ? 1
          : boundedInteger(command.start, "start", 0, Number.MAX_SAFE_INTEGER - count);
        const chunk: string[] = [];
        for (let offset = 0; offset < count; offset++) {
          chunk.push(`${prefix}${start + offset}\r\n`);
          if (chunk.length === 256) {
            await writeOutput(chunk.join(""));
            chunk.length = 0;
          }
        }
        if (chunk.length > 0) await writeOutput(chunk.join(""));
        return;
      }
      case "REPORT_SIZE": {
        const nonce = requiredString(command.nonce, "nonce");
        // Bun's process.stdout dimensions remain at their spawn values after
        // Bun.Terminal.resize. A fresh WriteStream performs the OS terminal
        // size query, proving the child actually observes TIOCSWINSZ/ConPTY.
        const output = new WriteStream(1);
        await writeOutput(`SIZE:${nonce}:${output.columns ?? 0}x${output.rows ?? 0}\r\n`);
        return;
      }
      case "ALT_SCREEN": {
        const nonce = command.nonce === undefined ? "ready" : requiredString(command.nonce, "nonce");
        if (typeof command.active !== "boolean") throw new Error("active must be a boolean");
        altScreenActive = command.active;
        if (!command.active) {
          await writeOutput(`\x1b[?1049lALT_EXIT:${nonce}\r\n`);
          return;
        }
        const prefix = command.prefix === undefined ? "ALTLINE-" : requiredString(command.prefix, "prefix");
        const count = command.count === undefined ? 20 : boundedInteger(command.count, "count", 1, 200);
        let output = "\x1b[?1049h\x1b[H\x1b[2J";
        for (let index = 1; index <= count; index++) output += `${prefix}${index}\r\n`;
        output += `ALT_READY:${nonce}\r\n`;
        await writeOutput(output);
        return;
      }
      case "SPAWN_TREE": {
        const nonce = requiredString(command.nonce, "nonce");
        const depth = command.depth === undefined ? 2 : boundedInteger(command.depth, "depth", 0, 8);
        const child = spawnTreeChild(depth);
        treeChildren.push(child);
        await writeOutput(`TREE:${nonce}:${child.pid}\r\n`);
        return;
      }
      case "EXIT":
        restoreInput();
        process.exit(0);
    }
  };

  const queueWork = (work: () => Promise<void>): void => {
    pendingWork += 1;
    commandLane = commandLane
      .then(work)
      .catch(async (error) => {
        const message = error instanceof Error ? error.message : String(error);
        await writeOutput(`FIXTURE_ERROR:${message.replace(/[\r\n]/g, " ").slice(0, 240)}\r\n`);
      })
      .finally(() => {
        pendingWork -= 1;
        if (inputFailed) return;
        consumeInput();
        if (pendingWork < MAX_PENDING_WORK && bufferedInput.length < MAX_BUFFERED_INPUT_BYTES) {
          process.stdin.resume();
        }
      });
  };

  const queueFixtureError = (error: unknown): void => {
    queueWork(() => Promise.reject(error));
  };

  const failInput = (message: string): void => {
    if (inputFailed) return;
    inputFailed = true;
    armedInput = null;
    discardNextLineFeed = false;
    bufferedInput = Buffer.alloc(0);
    payloadBytes = -1;
    process.stdin.pause();
    queueWork(async () => {
      await writeOutput(`FIXTURE_ERROR:${message}\r\n`);
      restoreInput();
      process.exit(1);
    });
  };

  const consumeRawInputByte = (): void => {
    const byte = bufferedInput[0]!;
    if (discardNextLineFeed) {
      discardNextLineFeed = false;
      if (byte === newlineByte) {
        bufferedInput = bufferedInput.subarray(1);
        return;
      }
    }

    const armed = armedInput;
    if (armed === null) {
      bufferedInput = bufferedInput.subarray(1);
      return;
    }
    const waitsForLine = armed.kind === "line-overwrite"
      || (armed.kind === "alt-redraw" && armed.trigger === "line");
    if (!waitsForLine) {
      armedInput = null;
      bufferedInput = bufferedInput.subarray(1);
      queueWork(() => handleArmedTrigger(armed));
      return;
    }

    if (byte === 0x0d || byte === newlineByte) {
      armedInput = null;
      if (byte === 0x0d && bufferedInput[1] === newlineByte) {
        bufferedInput = bufferedInput.subarray(2);
      } else {
        bufferedInput = bufferedInput.subarray(1);
        discardNextLineFeed = byte === 0x0d;
      }
      queueWork(() => handleArmedTrigger(armed));
      return;
    }

    bufferedInput = bufferedInput.subarray(1);
    armed.bytes += 1;
    if (armed.bytes > MAX_ARMED_LINE_BYTES) {
      armedInput = null;
      queueFixtureError(new Error(`armed line exceeds ${MAX_ARMED_LINE_BYTES} bytes`));
    }
  };

  function consumeInput(): void {
    for (;;) {
      if (pendingWork >= MAX_PENDING_WORK) {
        process.stdin.pause();
        return;
      }
      if (payloadBytes >= 0) {
        if (bufferedInput.length < payloadBytes) return;
        const payload = bufferedInput.subarray(0, payloadBytes);
        bufferedInput = bufferedInput.subarray(payloadBytes);
        payloadBytes = -1;
        try {
          // Arming must take effect while this input chunk is being parsed: the
          // trigger byte may immediately follow the frame. Output remains ordered
          // on commandLane, so the ready marker is still written before its effect.
          const raw = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(payload));
          const nextArmedInput = armedInputForCommand(raw);
          if (nextArmedInput !== null) armedInput = nextArmedInput;
          queueWork(() => handleCommand(raw));
        } catch (error) {
          queueFixtureError(error);
        }
        continue;
      }
      if (bufferedInput.length === 0) return;
      if (bufferedInput.length < protocolBytes.length) {
        if (protocolBytes.subarray(0, bufferedInput.length).equals(bufferedInput)) return;
        consumeRawInputByte();
        continue;
      }
      if (!bufferedInput.subarray(0, protocolBytes.length).equals(protocolBytes)) {
        consumeRawInputByte();
        continue;
      }
      const newline = bufferedInput.indexOf(newlineByte, protocolBytes.length);
      if (newline < 0) {
        if (bufferedInput.length <= protocolBytes.length + 16) return;
        consumeRawInputByte();
        continue;
      }
      const lengthText = bufferedInput.subarray(protocolBytes.length, newline).toString("ascii");
      const length = Number.parseInt(lengthText, 10);
      bufferedInput = bufferedInput.subarray(newline + 1);
      if (!/^\d+$/.test(lengthText) || !Number.isSafeInteger(length) || length < 2 || length > MAX_FRAME_BYTES) {
        queueFixtureError(new Error("invalid frame length"));
        continue;
      }
      payloadBytes = length;
    }
  }

  const ingestInput = (incoming: Buffer | Uint8Array): void => {
    let offset = 0;
    while (offset < incoming.length && !inputFailed) {
      const capacity = MAX_BUFFERED_INPUT_BYTES - bufferedInput.length;
      if (capacity === 0) {
        const retainedBytes = bufferedInput.length;
        consumeInput();
        if (bufferedInput.length >= retainedBytes) {
          failInput(`input buffer exceeds ${MAX_BUFFERED_INPUT_BYTES} bytes`);
          return;
        }
        continue;
      }
      const take = Math.min(capacity, incoming.length - offset);
      const portion = incoming.subarray(offset, offset + take);
      bufferedInput = bufferedInput.length === 0
        ? Buffer.from(portion)
        : Buffer.concat([bufferedInput, portion], bufferedInput.length + take);
      offset += take;
      consumeInput();
    }
    if (pendingWork >= MAX_PENDING_WORK || bufferedInput.length >= MAX_BUFFERED_INPUT_BYTES) {
      process.stdin.pause();
    }
  };

  process.stdin.on("data", ingestInput);
  process.stdin.on("end", () => process.exit(0));
  process.on("SIGINT", () => process.exit(0));
  process.on("SIGTERM", () => process.exit(0));
  process.on("exit", restoreInput);
  void writeOutput(`${PTY_FIXTURE_READY}\r\n`);
}

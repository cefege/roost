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
const protocolBytes = Buffer.from(PTY_FIXTURE_PROTOCOL, "ascii");
const newlineByte = 0x0a;
const treeChildren: Array<{ pid: number; kill(signal?: number | NodeJS.Signals): void }> = [];
let restored = false;
let armedNonce: string | null = null;
let bufferedInput = Buffer.alloc(0);
let payloadBytes = -1;
let commandLane = Promise.resolve();

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

  const handleCommand = async (raw: unknown): Promise<void> => {
    if (!raw || typeof raw !== "object" || !("op" in raw) || typeof raw.op !== "string") {
      throw new Error("command must contain an op string");
    }
    const command = raw as PtyFixtureCommand;
    switch (command.op) {
      case "ARM_KEY": {
        armedNonce = requiredString(command.nonce, "nonce");
        await writeOutput(`ARMED:${armedNonce}\r\n`);
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

  const queueCommand = (raw: unknown): void => {
    commandLane = commandLane
      .then(() => handleCommand(raw))
      .catch(async (error) => {
        const message = error instanceof Error ? error.message : String(error);
        await writeOutput(`FIXTURE_ERROR:${message.replace(/[\r\n]/g, " ").slice(0, 240)}\r\n`);
      });
  };

  const consumeInput = (): void => {
    for (;;) {
      if (armedNonce !== null && bufferedInput.length > 0) {
        const nonce = armedNonce;
        armedNonce = null;
        bufferedInput = bufferedInput.subarray(1);
        queueCommand({ op: "EMIT", text: `ACK:${nonce}` });
        continue;
      }
      if (payloadBytes >= 0) {
        if (bufferedInput.length < payloadBytes) return;
        const payload = bufferedInput.subarray(0, payloadBytes);
        bufferedInput = bufferedInput.subarray(payloadBytes);
        payloadBytes = -1;
        try {
          queueCommand(JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(payload)));
        } catch (error) {
          queueCommand({
            op: "EMIT",
            text: `FIXTURE_ERROR:${String(error).replace(/[\r\n]/g, " ").slice(0, 240)}`,
          });
        }
        continue;
      }
      if (bufferedInput.length === 0) return;
      if (bufferedInput.length < protocolBytes.length) {
        if (protocolBytes.subarray(0, bufferedInput.length).equals(bufferedInput)) return;
        bufferedInput = bufferedInput.subarray(1);
        continue;
      }
      if (!bufferedInput.subarray(0, protocolBytes.length).equals(protocolBytes)) {
        bufferedInput = bufferedInput.subarray(1);
        continue;
      }
      const newline = bufferedInput.indexOf(newlineByte, protocolBytes.length);
      if (newline < 0) {
        if (bufferedInput.length > protocolBytes.length + 16) bufferedInput = bufferedInput.subarray(1);
        return;
      }
      const lengthText = bufferedInput.subarray(protocolBytes.length, newline).toString("ascii");
      const length = Number.parseInt(lengthText, 10);
      bufferedInput = bufferedInput.subarray(newline + 1);
      if (!/^\d+$/.test(lengthText) || !Number.isSafeInteger(length) || length < 2 || length > MAX_FRAME_BYTES) {
        queueCommand({ op: "EMIT", text: "FIXTURE_ERROR:invalid frame length" });
        continue;
      }
      payloadBytes = length;
    }
  };

  process.stdin.on("data", (chunk: Buffer | Uint8Array) => {
    const incoming = Buffer.from(chunk);
    bufferedInput = bufferedInput.length === 0
      ? incoming
      : Buffer.concat([bufferedInput, incoming], bufferedInput.length + incoming.length);
    consumeInput();
  });
  process.stdin.on("end", () => process.exit(0));
  process.on("SIGINT", () => process.exit(0));
  process.on("SIGTERM", () => process.exit(0));
  process.on("exit", restoreInput);
  void writeOutput(`${PTY_FIXTURE_READY}\r\n`);
}

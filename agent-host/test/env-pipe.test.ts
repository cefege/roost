import { once } from "node:events";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { WebSocket, WebSocketServer } from "ws";
import { expect, test } from "vitest";

const pipePath = fileURLToPath(new URL("../src/env-pipe.ts", import.meta.url));

test("pipes daemon upload and opaque stdin/stdout frames over the internal WebSocket", async () => {
  const directory = await mkdtemp(join(tmpdir(), "roost-agent-pipe-"));
  const daemonPath = join(directory, "pi-env");
  const daemonBytes = Buffer.from("daemon binary bytes");
  await writeFile(daemonPath, daemonBytes);
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await once(server, "listening");
  const port = (server.address() as { port: number }).port;
  const workerSocketPromise = once(server, "connection");
  const secret = "0123456789abcdef0123456789abcdef";
  const child = spawn(process.execPath, [pipePath, "worker-fp", "serve", "--token", "a".repeat(32)], {
    env: { ...process.env, ROOST_AGENT_HOST_SECRET: secret, ROOST_COORDINATOR_INTERNAL_URL: `ws://127.0.0.1:${port}`, ROOST_PI_ENV_DAEMONS: JSON.stringify({ "linux-x64": { sha256: "b".repeat(64), path: daemonPath } }) },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const [workerSocket] = await workerSocketPromise as [WebSocket];
  workerSocket.binaryType = "nodebuffer";
  try {
    const [openData, openBinary] = await once(workerSocket, "message") as [Buffer, boolean];
    expect(openBinary).toBe(false);
    expect(JSON.parse(openData.toString())).toEqual({ type: "open", args: ["serve", "--token", "a".repeat(32)], daemons: { "linux-x64": "b".repeat(64) } });
    workerSocket.send(JSON.stringify({ type: "need_daemon", platform: "linux-x64" }));
    const [beginData, beginBinary] = await once(workerSocket, "message") as [Buffer, boolean];
    expect(beginBinary).toBe(false);
    expect(JSON.parse(beginData.toString())).toEqual({ type: "daemon_chunk_begin", size: daemonBytes.length });
    const [daemonChunk, chunkIsBinary] = await once(workerSocket, "message") as [Buffer, boolean];
    expect(chunkIsBinary).toBe(true);
    expect(daemonChunk).toEqual(await readFile(daemonPath));
    const [endData, endBinary] = await once(workerSocket, "message") as [Buffer, boolean];
    expect(endBinary).toBe(false);
    expect(JSON.parse(endData.toString())).toEqual({ type: "daemon_end" });
    workerSocket.send(JSON.stringify({ type: "opened" }));
    const childOutput = once(child.stdout, "data") as Promise<[Buffer]>;
    workerSocket.send(Buffer.from("opaque stdout"));
    expect((await childOutput)[0].toString()).toBe("opaque stdout");
    const inputFrame = once(workerSocket, "message") as Promise<[Buffer, boolean]>;
    child.stdin.write("opaque stdin");
    const [inputData, inputBinary] = await inputFrame;
    expect(inputBinary).toBe(true);
    expect(inputData.toString()).toBe("opaque stdin");
    const childExit = once(child, "exit") as Promise<[number | null]>;
    workerSocket.close(1000, "exit 0");
    expect((await childExit)[0]).toBe(0);
  } finally {
    if (workerSocket.readyState === WebSocket.OPEN) workerSocket.close(1000, "exit 0");
    if (child.exitCode === null) child.kill();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    await rm(directory, { recursive: true, force: true });
  }
});

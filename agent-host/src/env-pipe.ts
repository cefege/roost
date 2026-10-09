import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { once } from "node:events";
import { WebSocket, type RawData } from "ws";

const workerFp = process.argv[2];
const pipeArgs = process.argv.slice(3);
const secret = process.env.ROOST_AGENT_HOST_SECRET;
const base = process.env.ROOST_COORDINATOR_INTERNAL_URL ?? "ws://127.0.0.1:4113";
const daemons = JSON.parse(process.env.ROOST_PI_ENV_DAEMONS ?? "{}") as Record<string, { sha256: string; path: string }>;
if (!workerFp || !secret) throw new Error("worker fingerprint and host secret are required");

const url = new URL(`/internal/agent-env/${encodeURIComponent(workerFp)}`, base);
url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
const socket = new WebSocket(url, { headers: { Authorization: `Bearer ${secret}` }, maxPayload: 64 * 1024 * 1024 });
socket.binaryType = "nodebuffer";
let started = false;

function rawBuffer(data: RawData): Buffer {
  if (Array.isArray(data)) return Buffer.concat(data);
  if (data instanceof ArrayBuffer) return Buffer.from(new Uint8Array(data));
  return Buffer.from(data);
}
function sendJson(value: unknown): void {
  if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value));
}

socket.on("open", () => {
  sendJson({ type: "open", args: pipeArgs, daemons: Object.fromEntries(Object.entries(daemons).map(([key, value]) => [key, value.sha256])) });
});
socket.on("message", async (data, isBinary) => {
  if (isBinary) {
    const bytes = rawBuffer(data);
    if (started && !process.stdout.write(bytes)) await once(process.stdout, "drain");
    return;
  }
  let message: Record<string, unknown>;
  try { message = JSON.parse(data.toString()) as Record<string, unknown>; }
  catch { socket.close(1008, "invalid message"); return; }
  if (message.type === "need_daemon") {
    const daemon = daemons[String(message.platform)];
    if (!daemon) { socket.close(1011, `no pi-env daemon for ${String(message.platform)}`); return; }
    sendJson({ type: "daemon_chunk_begin", size: (await stat(daemon.path)).size });
    for await (const chunk of createReadStream(daemon.path, { highWaterMark: 256 * 1024 })) {
      if (socket.readyState !== WebSocket.OPEN) return;
      socket.send(chunk);
    }
    sendJson({ type: "daemon_end" });
  } else if (message.type === "opened") {
    started = true;
    process.stdin.on("data", chunk => { if (socket.readyState === WebSocket.OPEN) socket.send(chunk); });
    process.stdin.on("end", () => socket.close(1000));
  } else if (message.type === "stderr" && typeof message.text === "string") {
    process.stderr.write(message.text);
  }
});
socket.on("close", (code, reason) => {
  const text = reason.toString();
  process.exitCode = code === 1000 && text === "exit 0" ? 0 : 1;
  if (text && text !== "exit 0") process.stderr.write(`${text}\n`);
  process.exit();
});
socket.on("error", error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });

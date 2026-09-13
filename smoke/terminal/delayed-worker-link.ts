// Loopback TCP reverse proxy for delayed terminal smoke worker links.
// Stack-owned fixture workers use it to exercise only worker↔coordinator wire latency.
// It tunnels boot HTTP untouched and queues complete WebSocket frames only after a 101 upgrade.
// Socket-pair ownership makes stop deterministic and prevents timer or connection leaks.

import { Buffer } from "node:buffer";
import { createConnection, createServer, type Socket } from "node:net";

const HTTP_HEADER_END = Buffer.from("\r\n\r\n");
const EMPTY_BUFFER: Buffer<ArrayBufferLike> = Buffer.alloc(0);
const MAX_FRAME_BYTES = 4 * 1024 * 1024;
const MAX_DIRECTION_QUEUE_BYTES = 8 * 1024 * 1024;
const MAX_HTTP_HEADER_BYTES = 64 * 1024;

type SocketSide = "client" | "target";
type TargetEndpoint = { host: string; port: number };
type FrameInspection = { frameBytes: number } | "incomplete" | "oversized";
type QueuedFrame = { bytes: Buffer; dueAtMs: number };

export type DelayedWorkerLinkOptions = {
  targetUrl: string;
  oneWayDelayMs: 0 | 25;
};

export interface DelayedWorkerLink {
  url: string;
  stop(): Promise<void>;
}

export async function startDelayedWorkerLink(
  options: DelayedWorkerLinkOptions,
): Promise<DelayedWorkerLink> {
  if (options.oneWayDelayMs !== 0 && options.oneWayDelayMs !== 25) {
    throw new Error("delayed worker link only accepts a 0 ms or 25 ms one-way delay");
  }
  let parsedTarget: URL;
  try {
    parsedTarget = new URL(options.targetUrl);
  } catch {
    throw new Error(`invalid delayed worker link target URL: ${options.targetUrl}`);
  }
  if (parsedTarget.protocol !== "http:") {
    throw new Error("delayed worker link requires an http target URL");
  }
  const target: TargetEndpoint = {
    host: parsedTarget.hostname,
    port: parsedTarget.port === "" ? 80 : Number(parsedTarget.port),
  };
  const connections = new Set<SocketPair>();
  let stopping = false;
  let stopPromise: Promise<void> | undefined;
  const server = createServer({ allowHalfOpen: true });
  server.on("connection", (clientSocket) => {
    if (stopping) {
      clientSocket.destroy();
      return;
    }
    const targetSocket = createConnection({ host: target.host, port: target.port, allowHalfOpen: true });
    let connection: SocketPair;
    connection = new SocketPair(
      clientSocket,
      targetSocket,
      options.oneWayDelayMs,
      () => connections.delete(connection),
    );
    connections.add(connection);
    connection.start();
  });
  await new Promise<void>((resolve, reject) => {
    const fail = (error: Error) => {
      server.off("listening", ready);
      reject(error);
    };
    const ready = () => {
      server.off("error", fail);
      resolve();
    };
    server.once("error", fail);
    server.once("listening", ready);
    server.listen({ host: "127.0.0.1", port: 0 });
  });
  const stop = (): Promise<void> => {
    stopPromise ??= new Promise<void>((resolve) => {
      stopping = true;
      for (const connection of connections) connection.close();
      try {
        server.close(() => resolve());
      } catch {
        resolve();
      }
    });
    return stopPromise;
  };
  const address = server.address();
  if (!address || typeof address === "string") {
    await stop();
    throw new Error("delayed worker link did not bind a TCP port");
  }
  server.on("error", () => { void stop(); });
  return { url: `http://127.0.0.1:${address.port}`, stop };
}

class SocketPair {
  readonly #clientToTarget: DelayedFrameStream;
  readonly #targetToClient: DelayedFrameStream;
  readonly #client: Socket;
  readonly #target: Socket;
  readonly #onClosed: () => void;
  #clientRequestHeaders = EMPTY_BUFFER;
  #clientRequestInspected = false;
  #targetResponseHeaders = EMPTY_BUFFER;
  #paused: Record<SocketSide, boolean> = { client: false, target: false };
  #writeEnded: Record<SocketSide, boolean> = { client: false, target: false };
  #upgradeRequested = false;
  #webSocketOpen = false;
  #closed = false;

  constructor(client: Socket, target: Socket, oneWayDelayMs: 0 | 25, onClosed: () => void) {
    this.#client = client; this.#target = target; this.#onClosed = onClosed;
    this.#clientToTarget = new DelayedFrameStream(this.#target, oneWayDelayMs, () => this.close());
    this.#targetToClient = new DelayedFrameStream(this.#client, oneWayDelayMs, () => this.close());
  }

  start(): void {
    this.#client.on("data", (chunk: Buffer) => this.#onClientData(chunk));
    this.#target.on("data", (chunk: Buffer) => this.#onTargetData(chunk));
    this.#client.on("end", () => this.#onEnd("client"));
    this.#target.on("end", () => this.#onEnd("target"));
    for (const socket of [this.#client, this.#target]) {
      socket.on("error", () => this.close());
      socket.on("close", () => this.close());
    }
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#clientToTarget.stop();
    this.#targetToClient.stop();
    this.#clientRequestHeaders = EMPTY_BUFFER;
    this.#targetResponseHeaders = EMPTY_BUFFER;
    this.#onClosed();
    this.#client.destroy();
    this.#target.destroy();
  }

  #onClientData(chunk: Buffer): void {
    if (this.#closed) return;
    if (this.#webSocketOpen) return this.#clientToTarget.receive(chunk);
    this.#trackUpgradeRequest(chunk);
    this.#forwardImmediately("client", chunk);
  }

  #onTargetData(chunk: Buffer): void {
    if (this.#closed) return;
    if (this.#webSocketOpen) return this.#targetToClient.receive(chunk);
    if (!this.#upgradeRequested) { this.#clientRequestInspected = false; return this.#forwardImmediately("target", chunk); }
    this.#forwardUpgradeResponse(chunk);
  }

  #onEnd(source: SocketSide): void {
    if (this.#closed) return;
    if (!this.#webSocketOpen) return this.#endPeerWrite(source);
    const stream = source === "client" ? this.#clientToTarget : this.#targetToClient;
    if (!stream.finish(() => this.#endPeerWrite(source))) this.close();
  }

  #trackUpgradeRequest(chunk: Buffer): void {
    if (this.#upgradeRequested || this.#clientRequestInspected) return;
    const headers = this.#clientRequestHeaders.byteLength === 0
      ? Buffer.from(chunk)
      : Buffer.concat([this.#clientRequestHeaders, chunk]);
    const headerEnd = headers.indexOf(HTTP_HEADER_END);
    if (headerEnd < 0) {
      if (headers.byteLength > MAX_HTTP_HEADER_BYTES) return this.close();
      this.#clientRequestHeaders = headers;
      return;
    }
    if (headerEnd + HTTP_HEADER_END.byteLength > MAX_HTTP_HEADER_BYTES) return this.close();
    this.#clientRequestInspected = true;
    this.#clientRequestHeaders = EMPTY_BUFFER;
    const text = headers.subarray(0, headerEnd).toString("latin1");
    this.#upgradeRequested = /(?:^|\r\n)upgrade\s*:\s*websocket\s*(?:\r\n|$)/i.test(text)
      && /(?:^|\r\n)connection\s*:[^\r\n]*\bupgrade\b/i.test(text)
      && /(?:^|\r\n)sec-websocket-key\s*:\s*\S/i.test(text);
  }

  #forwardUpgradeResponse(chunk: Buffer): void {
    const priorBytes = this.#targetResponseHeaders.byteLength;
    const headers = priorBytes === 0
      ? Buffer.from(chunk)
      : Buffer.concat([this.#targetResponseHeaders, chunk]);
    const headerEnd = headers.indexOf(HTTP_HEADER_END);
    if (headerEnd < 0) {
      if (headers.byteLength > MAX_HTTP_HEADER_BYTES) return this.close();
      this.#targetResponseHeaders = headers;
      return this.#forwardImmediately("target", chunk);
    }
    if (headerEnd + HTTP_HEADER_END.byteLength > MAX_HTTP_HEADER_BYTES) return this.close();
    const headerBytes = headerEnd + HTTP_HEADER_END.byteLength;
    const bytesFromChunk = Math.min(chunk.byteLength, Math.max(0, headerBytes - priorBytes));
    const responseHeaders = headers.subarray(0, headerBytes);
    const statusEnd = responseHeaders.indexOf("\r\n");
    const switchingProtocols = statusEnd >= 0
      && /^HTTP\/\d\.\d\s+101(?:\s|$)/i.test(responseHeaders.subarray(0, statusEnd).toString("latin1"));
    this.#targetResponseHeaders = EMPTY_BUFFER;
    if (switchingProtocols) this.#webSocketOpen = true;
    if (bytesFromChunk > 0) this.#forwardImmediately("target", chunk.subarray(0, bytesFromChunk));
    if (this.#closed) return;
    const remainder = chunk.subarray(bytesFromChunk);
    if (!switchingProtocols) {
      this.#upgradeRequested = false;
      if (remainder.byteLength > 0) this.#forwardImmediately("target", remainder);
      return;
    }
    if (remainder.byteLength > 0) this.#targetToClient.receive(remainder);
  }

  #forwardImmediately(source: SocketSide, bytes: Buffer): void {
    const destination = source === "client" ? this.#target : this.#client;
    const sourceSocket = source === "client" ? this.#client : this.#target;
    if (destination.destroyed || !destination.writable) return this.close();
    try {
      if (destination.write(bytes)) return;
    } catch {
      this.close();
      return;
    }
    if (this.#paused[source]) return;
    this.#paused[source] = true;
    sourceSocket.pause();
    destination.once("drain", () => {
      this.#paused[source] = false;
      if (!this.#closed) sourceSocket.resume();
    });
  }

  #endPeerWrite(source: SocketSide): void {
    const peer = source === "client" ? "target" : "client";
    if (this.#closed || this.#writeEnded[peer]) return;
    this.#writeEnded[peer] = true;
    try {
      (peer === "client" ? this.#client : this.#target).end();
    } catch {
      this.close();
    }
  }
}

class DelayedFrameStream {
  #pending = EMPTY_BUFFER;
  #frames: QueuedFrame[] = [];
  #queuedBytes = 0;
  #timer: NodeJS.Timeout | undefined;
  #writing = false;
  #finishing = false;
  #stopped = false;
  #onDrained: (() => void) | undefined;
  readonly #destination: Socket;
  readonly #oneWayDelayMs: 0 | 25;
  readonly #closePair: () => void;

  constructor(destination: Socket, oneWayDelayMs: 0 | 25, closePair: () => void) {
    this.#destination = destination; this.#oneWayDelayMs = oneWayDelayMs; this.#closePair = closePair;
  }

  receive(chunk: Buffer): void {
    if (this.#stopped || this.#finishing) return this.#closePair();
    let bytes = chunk;
    let bytesAreOwned = false;
    if (this.#pending.byteLength > 0) {
      bytes = Buffer.concat([this.#pending, chunk]);
      bytesAreOwned = true;
      this.#pending = EMPTY_BUFFER;
    }
    let offset = 0;
    while (offset < bytes.byteLength) {
      const inspection = inspectWebSocketFrame(bytes, offset);
      if (inspection === "oversized") return this.#closePair();
      if (inspection === "incomplete") {
        this.#holdPending(bytes.subarray(offset), bytesAreOwned);
        return;
      }
      const frame = bytes.subarray(offset, offset + inspection.frameBytes);
      if (!this.#enqueue(bytesAreOwned ? frame : Buffer.from(frame))) return;
      offset += inspection.frameBytes;
    }
    this.#pending = EMPTY_BUFFER;
  }

  finish(onDrained: () => void): boolean {
    if (this.#pending.byteLength > 0) return false;
    this.#finishing = true;
    this.#onDrained = onDrained;
    this.#finishIfDrained();
    return true;
  }

  stop(): void {
    this.#stopped = true;
    clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#pending = EMPTY_BUFFER;
    this.#frames = [];
    this.#queuedBytes = 0;
    this.#onDrained = undefined;
  }
  #holdPending(bytes: Buffer, alreadyOwned: boolean): void {
    const pending = alreadyOwned ? bytes : Buffer.from(bytes);
    if (pending.byteLength > MAX_DIRECTION_QUEUE_BYTES - this.#queuedBytes) {
      this.#closePair();
      return;
    }
    this.#pending = pending;
  }


  #enqueue(ownedFrame: Buffer): boolean {
    if (ownedFrame.byteLength > MAX_DIRECTION_QUEUE_BYTES - this.#queuedBytes) {
      this.#closePair();
      return false;
    }
    this.#queuedBytes += ownedFrame.byteLength;
    this.#frames.push({ bytes: ownedFrame, dueAtMs: Date.now() + this.#oneWayDelayMs });
    this.#schedule();
    return true;
  }

  #schedule(): void {
    if (this.#stopped || this.#writing || this.#timer || this.#frames.length === 0) return;
    const delayMs = Math.max(0, this.#frames[0]!.dueAtMs - Date.now());
    if (delayMs === 0) return this.#writeHead();
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      this.#writeHead();
    }, delayMs);
  }

  #writeHead(): void {
    if (this.#stopped || this.#writing) return;
    const frame = this.#frames[0];
    if (!frame) return this.#finishIfDrained();
    if (this.#destination.destroyed || !this.#destination.writable) return this.#closePair();
    this.#writing = true;
    try {
      this.#destination.write(frame.bytes, (error) => {
        if (this.#stopped) return;
        this.#writing = false;
        if (error) return this.#closePair();
        const sent = this.#frames.shift();
        if (!sent) return this.#closePair();
        this.#queuedBytes -= sent.bytes.byteLength;
        this.#finishIfDrained();
        this.#schedule();
      });
    } catch {
      this.#writing = false;
      this.#closePair();
    }
  }

  #finishIfDrained(): void {
    if (!this.#finishing || this.#writing || this.#timer || this.#frames.length > 0) return;
    const onDrained = this.#onDrained;
    this.#onDrained = undefined;
    if (onDrained) onDrained();
  }
}

function inspectWebSocketFrame(bytes: Buffer, offset: number): FrameInspection {
  const available = bytes.byteLength - offset;
  if (available < 2) return "incomplete";
  const secondByte = bytes[offset + 1]!;
  const lengthMarker = secondByte & 0x7f;
  let headerBytes = 2;
  let payloadBytes: number;
  if (lengthMarker === 126) {
    if (available < 4) return "incomplete";
    payloadBytes = bytes.readUInt16BE(offset + 2);
    headerBytes += 2;
  } else if (lengthMarker === 127) {
    if (available < 10) return "incomplete";
    const highBits = bytes.readUInt32BE(offset + 2);
    const lowBits = bytes.readUInt32BE(offset + 6);
    if (highBits !== 0 || lowBits > MAX_FRAME_BYTES) return "oversized";
    payloadBytes = lowBits;
    headerBytes += 8;
  } else {
    payloadBytes = lengthMarker;
  }
  if ((secondByte & 0x80) !== 0) headerBytes += 4;
  if (headerBytes + payloadBytes > MAX_FRAME_BYTES) return "oversized";
  if (available < headerBytes + payloadBytes) return "incomplete";
  return { frameBytes: headerBytes + payloadBytes };
}

// Loopback TCP reverse proxy for delayed terminal smoke worker links.
// Stack-owned fixture workers use it to exercise only worker↔coordinator wire latency.
// It tunnels boot HTTP untouched and queues complete WebSocket frames only after a 101 upgrade.
// Socket-pair ownership makes stop deterministic and prevents timer or connection leaks.

import { Buffer } from "node:buffer";
import { createConnection, createServer, type Socket } from "node:net";
import type { CoordWorkerUp } from "../../apps/shared/src/gen/roost/v1/worker_transport_pb.ts";
import {
  DelayedFrameStream,
  type WorkerFrameFilter,
} from "./delayed-worker-frames.ts";

const HTTP_HEADER_END = Buffer.from("\r\n\r\n");
const EMPTY_BUFFER: Buffer<ArrayBufferLike> = Buffer.alloc(0);
const MAX_HTTP_HEADER_BYTES = 64 * 1024;

type SocketSide = "client" | "target";
type TargetEndpoint = { host: string; port: number };

export type DelayedWorkerLinkOptions = {
  targetUrl: string;
  oneWayDelayMs: 0 | 25;
  workerFrameFilter?: (frame: CoordWorkerUp) => boolean;
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
  let filterFailure: Error | undefined;
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
      options.workerFrameFilter,
      () => connections.delete(connection),
      (error) => { filterFailure ??= error; },
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
  const stop = async (): Promise<void> => {
    stopPromise ??= new Promise<void>((resolve) => {
      stopping = true;
      for (const connection of connections) connection.close();
      try {
        server.close(() => resolve());
      } catch {
        resolve();
      }
    });
    await stopPromise;
    if (filterFailure) throw filterFailure;
  };
  const address = server.address();
  if (!address || typeof address === "string") {
    await stop();
    throw new Error("delayed worker link did not bind a TCP port");
  }
  server.on("error", () => { void stop().catch(() => undefined); });
  return { url: `http://127.0.0.1:${address.port}`, stop };
}

class SocketPair {
  readonly #clientToTarget: DelayedFrameStream;
  readonly #targetToClient: DelayedFrameStream;
  readonly #client: Socket;
  readonly #target: Socket;
  readonly #onClosed: () => void;
  readonly #onFilterFailure: (error: Error) => void;
  #clientRequestHeaders = EMPTY_BUFFER;
  #clientRequestInspected = false;
  #targetResponseHeaders = EMPTY_BUFFER;
  #paused: Record<SocketSide, boolean> = { client: false, target: false };
  #writeEnded: Record<SocketSide, boolean> = { client: false, target: false };
  #upgradeRequested = false;
  #webSocketOpen = false;
  #closed = false;

  constructor(
    client: Socket,
    target: Socket,
    oneWayDelayMs: 0 | 25,
    workerFrameFilter: WorkerFrameFilter | undefined,
    onClosed: () => void,
    onFilterFailure: (error: Error) => void,
  ) {
    this.#client = client;
    this.#target = target;
    this.#onClosed = onClosed;
    this.#onFilterFailure = onFilterFailure;
    this.#clientToTarget = new DelayedFrameStream(
      this.#target,
      oneWayDelayMs,
      () => this.close(),
      workerFrameFilter,
      (message) => this.#failWorkerFrameFilter(message),
    );
    this.#targetToClient = new DelayedFrameStream(
      this.#client,
      oneWayDelayMs,
      () => this.close(),
    );
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

  #failWorkerFrameFilter(message: string): void {
    this.#onFilterFailure(new Error(`delayed worker frame filter failed: ${message}`));
    this.close();
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


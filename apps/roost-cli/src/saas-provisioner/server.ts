/**
 * Serves authenticated provisioning operations over the root-only private Unix socket.
 * Runtime supplies the verifier, replay store, and privileged operation implementation.
 * Framing, signature freshness, and durable replay responses make retries safe and fail-closed.
 */

import { chmodSync, chownSync, lstatSync, mkdirSync, unlinkSync } from "node:fs";
import { dirname } from "node:path";
import { createServer, type Server, type Socket } from "node:net";
import { canonicalJson, type CanonicalJsonValue } from "../saas-auth/canonical-json.ts";
import { encodePrivateIpcFrame, readPrivateIpcFrame } from "../saas-auth/private-ipc-framing.ts";
import {
  assertPrivateIpcProofFresh,
  parsePrivateIpcEnvelope,
  verifyPrivateIpcEnvelope,
  PRIVATE_IPC_MAX_FRAME_BYTES,
  PRIVATE_IPC_TIMEOUT_MS,
  type PrivateIpcRequest,
  type PrivateIpcVerificationKey,
} from "../saas-auth/private-ipc.ts";
import { ProvisionerReplayStore } from "./replay-store.ts";

export interface ProvisionerOperationContext { nonce: string; issuedAtMs: number; requestBytesSha256?: string }
export type ProvisionerOperation = (request: PrivateIpcRequest, context: ProvisionerOperationContext) => CanonicalJsonValue | Promise<CanonicalJsonValue>;
export interface ProvisionerServerOptions {
  socketPath: string;
  verificationKey: PrivateIpcVerificationKey;
  replayStore: ProvisionerReplayStore;
  operation: ProvisionerOperation;
  now?: () => number;
  timeoutMs?: number;
}

const response = (value: CanonicalJsonValue): Buffer => Buffer.from(canonicalJson(value), "utf8");
const errorResponse = (error: string): Buffer => response({ ok: false, error });

export class ProvisionerIpcServer {
  readonly #options: ProvisionerServerOptions;
  readonly #inFlight = new Map<string, Promise<Buffer>>();
  #server: Server | null = null;

  constructor(options: ProvisionerServerOptions) { this.#options = options; }

  async listen(): Promise<void> {
    if (this.#server) throw new Error("provisioner IPC server is already listening");
    const parent = dirname(this.#options.socketPath);
    mkdirSync(parent, { recursive: true, mode: 0o750 });
    chmodSync(parent, 0o750);
    if (typeof process.getuid === "function" && process.getuid() === 0 && typeof process.getgid === "function") chownSync(parent, 0, process.getgid());
    try {
      const existing = lstatSync(this.#options.socketPath);
      if (!existing.isSocket()) throw new Error("refusing to replace non-socket provisioner IPC path");
      unlinkSync(this.#options.socketPath);
    } catch (error) {
      if (!(error && typeof error === "object" && "code" in error && error.code === "ENOENT")) throw error;
    }
    const server = createServer({ allowHalfOpen: true }, (socket) => this.#accept(socket));
    this.#server = server;
    const { promise, resolve, reject } = Promise.withResolvers<void>();
    server.once("listening", resolve);
    server.once("error", reject);
    server.listen(this.#options.socketPath);
    try { await promise; } catch (error) { this.#server = null; throw error; }
    chmodSync(this.#options.socketPath, 0o660);
    if (typeof process.getuid === "function" && process.getuid() === 0 && typeof process.getgid === "function") chownSync(this.#options.socketPath, 0, process.getgid());
  }

  async close(): Promise<void> {
    const server = this.#server;
    if (!server) return;
    this.#server = null;
    const { promise, resolve } = Promise.withResolvers<void>();
    server.close(() => resolve());
    await promise;
    try { if (lstatSync(this.#options.socketPath).isSocket()) unlinkSync(this.#options.socketPath); } catch {}
  }

  #accept(socket: Socket): void {
    const timeoutMs = this.#options.timeoutMs ?? PRIVATE_IPC_TIMEOUT_MS;
    const deadline = setTimeout(() => socket.destroy(), timeoutMs);
    void this.#handle(socket, timeoutMs).then((payload) => {
      if (!socket.destroyed) socket.end(encodePrivateIpcFrame(payload));
    }).catch(() => {
      if (!socket.destroyed) socket.end(encodePrivateIpcFrame(errorResponse("invalid-request")));
    }).finally(() => clearTimeout(deadline));
  }

  async #handle(socket: Socket, timeoutMs: number): Promise<Buffer> {
    const raw = await readPrivateIpcFrame(socket, { maxBytes: PRIVATE_IPC_MAX_FRAME_BYTES, timeoutMs });
    let envelope;
    try {
      envelope = parsePrivateIpcEnvelope(raw);
      const request = verifyPrivateIpcEnvelope(envelope, this.#options.verificationKey, (this.#options.now ?? Date.now)());
      assertPrivateIpcProofFresh(request, (this.#options.now ?? Date.now)());
    } catch {
      return errorResponse("invalid-request");
    }
    const request = { purpose: envelope.purpose, body: envelope.body } as PrivateIpcRequest;
    const reservation = this.#options.replayStore.reserve(envelope.nonce, raw, (this.#options.now ?? Date.now)());
    if (reservation.state === "mismatch") return errorResponse("replay-mismatch");
    if (reservation.state === "replay") return reservation.response;
    if (reservation.state === "pending") return this.#inFlight.get(envelope.nonce) ?? errorResponse("request-pending");

    const completion = (async () => {
      let payload: Buffer;
      try {
        const body = await this.#options.operation(request, { nonce: envelope.nonce, issuedAtMs: envelope.issuedAtMs });
        payload = response({ ok: true, body });
      } catch {
        payload = errorResponse("operation-failed");
      }
      if (payload.byteLength > PRIVATE_IPC_MAX_FRAME_BYTES) payload = errorResponse("operation-failed");
      this.#options.replayStore.complete(envelope.nonce, raw, payload, (this.#options.now ?? Date.now)());
      return payload;
    })();
    this.#inFlight.set(envelope.nonce, completion);
    try { return await completion; } finally { this.#inFlight.delete(envelope.nonce); }
  }
}

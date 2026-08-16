// Authenticated worker-side Hello probe for the multiplexed keeper.

import { Socket } from "node:net";
import type { LocalEndpoint } from "@roost/shared";
import {
  MuxFrameType,
  KEEPER_PROTOCOL_VERSION,
  SUPPORTED_KEEPER_FEATURES,
  decodeKeeperHelloResponse,
  decodeMuxFrames,
  encodeKeeperHelloRequest,
  encodeMuxFrame,
  isEmptyKeeperPayload,
  type KeeperFeature,
  type MuxFrame,
} from "./protocol-v2.ts";

export interface KeeperProbeResult {
  /** The endpoint accepted a transport connection. */
  reachable: boolean;
  /** The peer returned the strict post-capability-auth Hello response. */
  authenticated: boolean;
  /** Authentication, wire version, and every required feature matched. */
  compatible: boolean;
  keeperStamp?: string;
  features: readonly KeeperFeature[];
}

export interface AuthenticatedKeeperConnection extends KeeperProbeResult {
  reachable: true;
  authenticated: true;
  socket: Socket;
  /** Complete frames coalesced behind HelloResp in the same read. */
  pendingFrames: MuxFrame[];
  /** Partial frame bytes received behind HelloResp. */
  remaining: Buffer;
}

interface FailedKeeperConnection extends KeeperProbeResult {
  authenticated: false;
  socket?: never;
  pendingFrames?: never;
  remaining?: never;
}

export type KeeperConnectionAttempt = AuthenticatedKeeperConnection | FailedKeeperConnection;

function hasRequiredFeatures(features: readonly string[]): boolean {
  const available = new Set(features);
  return SUPPORTED_KEEPER_FEATURES.every(feature => available.has(feature));
}

/** Connect and perform the capability-bearing Hello as the first frame.
 * Successful sockets are returned paused so the caller can install its
 * long-lived frame listener without an intervening data event. */
export function connectKeeperAuthenticated(
  endpoint: LocalEndpoint,
  timeoutMs: number = 800,
): Promise<KeeperConnectionAttempt> {
  return new Promise<KeeperConnectionAttempt>((resolve) => {
    const socket = new Socket();
    let connected = false;
    let settled = false;
    let rxBuf = Buffer.alloc(0) as Buffer;

    const finishFailure = (reachable: boolean) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.removeListener("data", onData);
      socket.removeListener("connect", onConnect);
      socket.removeListener("close", onClose);
      socket.removeListener("error", onError);
      try { socket.destroy(); } catch { /* already closed */ }
      resolve({
        reachable,
        authenticated: false,
        compatible: !reachable,
        features: [],
      });
    };

    const onConnect = () => {
      connected = true;
      try {
        socket.write(encodeMuxFrame(
          MuxFrameType.Hello,
          0,
          encodeKeeperHelloRequest({
            version: KEEPER_PROTOCOL_VERSION,
            capability: endpoint.capability,
            features: [...SUPPORTED_KEEPER_FEATURES],
            pid: process.pid,
          }),
        ));
      } catch {
        finishFailure(true);
      }
    };

    const onData = (chunk: Buffer | Uint8Array) => {
      rxBuf = Buffer.concat([rxBuf, Buffer.from(chunk)]);
      let frames: MuxFrame[];
      let remaining: Buffer;
      try {
        ({ frames, remaining } = decodeMuxFrames(rxBuf));
      } catch {
        finishFailure(true);
        return;
      }
      if (frames.length === 0) {
        rxBuf = remaining;
        return;
      }
      const helloFrame = frames[0];
      if (helloFrame.type !== MuxFrameType.HelloResp || helloFrame.channelId !== 0) {
        finishFailure(true);
        return;
      }
      const hello = decodeKeeperHelloResponse(helloFrame.payload);
      if (!hello) {
        finishFailure(true);
        return;
      }
      settled = true;
      clearTimeout(timer);
      socket.removeListener("data", onData);
      socket.removeListener("connect", onConnect);
      socket.removeListener("close", onClose);
      socket.removeListener("error", onError);
      socket.pause();
      const features = hello.features.filter((feature): feature is KeeperFeature =>
        SUPPORTED_KEEPER_FEATURES.includes(feature as KeeperFeature));
      resolve({
        reachable: true,
        authenticated: true,
        compatible: hello.version === KEEPER_PROTOCOL_VERSION
          && hasRequiredFeatures(hello.features),
        keeperStamp: hello.build,
        features,
        socket,
        pendingFrames: frames.slice(1),
        remaining,
      });
    };

    const onClose = () => finishFailure(connected);
    const onError = () => finishFailure(connected);
    const timer = setTimeout(() => finishFailure(connected), timeoutMs);
    socket.once("connect", onConnect);
    socket.on("data", onData);
    socket.once("close", onClose);
    socket.once("error", onError);
    try {
      socket.connect(endpoint.address);
    } catch {
      finishFailure(false);
    }
  });
}

/** Probe without retaining the authenticated transport. */
export async function probeKeeperCompatible(
  endpoint: LocalEndpoint,
  timeoutMs: number = 800,
): Promise<KeeperProbeResult> {
  const attempt = await connectKeeperAuthenticated(endpoint, timeoutMs);
  if (attempt.authenticated) {
    try { attempt.socket.destroy(); } catch { /* already closed */ }
  }
  return {
    reachable: attempt.reachable,
    authenticated: attempt.authenticated,
    compatible: attempt.compatible,
    keeperStamp: attempt.keeperStamp,
    features: attempt.features,
  };
}

/** Ask an authenticated keeper to shut down. Version compatibility is not
 * required: this is the drain path for an old but capability-aware keeper. */
export async function shutdownKeeperAuthenticated(
  endpoint: LocalEndpoint,
  timeoutMs: number = 2_000,
): Promise<boolean> {
  const attempt = await connectKeeperAuthenticated(endpoint, timeoutMs);
  if (!attempt.authenticated) return false;
  const socket = attempt.socket;
  return new Promise<boolean>((resolve) => {
    let settled = false;
    let rxBuf = attempt.remaining;
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.removeListener("data", onData);
      socket.removeListener("close", onClose);
      socket.removeListener("error", onError);
      try { socket.destroy(); } catch { /* already closed */ }
      resolve(ok);
    };
    const onData = (chunk: Buffer | Uint8Array) => {
      rxBuf = Buffer.concat([rxBuf, Buffer.from(chunk)]);
      let frames: MuxFrame[];
      try {
        ({ frames, remaining: rxBuf } = decodeMuxFrames(rxBuf));
      } catch {
        finish(false);
        return;
      }
      for (const frame of frames) {
        if (frame.type === MuxFrameType.ShutdownAck
            && frame.channelId === 0
            && isEmptyKeeperPayload(frame.payload)) {
          finish(true);
          return;
        }
      }
    };
    const onClose = () => finish(false);
    const onError = () => finish(false);
    const timer = setTimeout(() => finish(false), timeoutMs);
    socket.on("data", onData);
    socket.once("close", onClose);
    socket.once("error", onError);
    try {
      socket.write(encodeMuxFrame(MuxFrameType.Shutdown, 0, new Uint8Array(0)));
      socket.resume();
    } catch {
      finish(false);
    }
  });
}

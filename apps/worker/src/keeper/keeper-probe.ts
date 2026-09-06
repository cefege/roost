// Authenticated worker-side keeper Hello probe and administrative shutdown.
// Probe facts keep transport reachability, authentication, wire compatibility,
// and exact KeeperContractV1 target equality independent.

import { Socket } from "node:net";
import { createHash } from "node:crypto";
import type { LocalEndpoint } from "@roost/shared/local-endpoint";
import {
  keeperBindingDigestInput,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import {
  MuxFrameType,
  KEEPER_PROTOCOL_VERSION,
  SUPPORTED_KEEPER_FEATURES,
  decodeKeeperHelloResponse,
  decodeMuxFrames,
  encodeKeeperHelloRequest,
  encodeMuxFrame,
  isEmptyKeeperPayload,
  type KeeperChannelBindingV1,
  type KeeperFeature,
  type MuxFrame,
} from "./protocol.ts";
import {
  KEEPER_TARGET_CONTRACT,
  keeperContractsExactlyEqual,
  keeperContractsProtocolCompatible,
} from "./keeper-stamp.ts";

export interface KeeperProbeResult {
  /** The endpoint accepted a transport connection. */
  reachable: boolean;
  /** The peer returned the strict post-capability-auth Hello response. */
  authenticated: boolean;
  /** Wire version and every worker-required feature matched. */
  protocolCompatible: boolean;
  /** Every target contract field matched; a missing digest is never exact. */
  exactTarget: boolean;
  contract?: KeeperContractV1;
  keeperPid?: number;
  processEpoch?: string;
  bindings?: readonly KeeperChannelBindingV1[];
  spawningChannels?: readonly number[];
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

export interface EmptyKeeperShutdownExpectation {
  keeperPid: number;
  processEpoch: string;
  bindingDigest: string;
}

export type KeeperConnectionAttempt =
  | AuthenticatedKeeperConnection
  | FailedKeeperConnection;


export function _keeperHelloProtocolCompatible(hello: {
  version: number;
  features: readonly string[];
  contract?: KeeperContractV1;
}): boolean {
  if (hello.contract === undefined) return false;
  const availableFeatures = new Set(hello.features);
  return hello.version === KEEPER_PROTOCOL_VERSION
    && KEEPER_TARGET_CONTRACT.required_features.every(
      feature => availableFeatures.has(feature),
    )
    && keeperContractsProtocolCompatible(
      KEEPER_TARGET_CONTRACT,
      hello.contract,
    );
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
        protocolCompatible: false,
        exactTarget: false,
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
      if (
        helloFrame.type !== MuxFrameType.HelloResp
        || helloFrame.channelId !== 0
      ) {
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
      const features = hello.features.filter(
        (feature): feature is KeeperFeature =>
          SUPPORTED_KEEPER_FEATURES.includes(feature as KeeperFeature),
      );
      const protocolCompatible = _keeperHelloProtocolCompatible(hello);
      resolve({
        reachable: true,
        authenticated: true,
        protocolCompatible,
        exactTarget: hello.contract !== undefined
          && keeperContractsExactlyEqual(
            KEEPER_TARGET_CONTRACT,
            hello.contract,
          ),
        contract: hello.contract,
        keeperPid: hello.pid,
        processEpoch: hello.process_epoch,
        bindings: hello.bindings,
        spawningChannels: hello.spawning_channels,
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
    protocolCompatible: attempt.protocolCompatible,
    exactTarget: attempt.exactTarget,
    contract: attempt.contract,
    keeperPid: attempt.keeperPid,
    processEpoch: attempt.processEpoch,
    bindings: attempt.bindings,
    spawningChannels: attempt.spawningChannels,
    features: attempt.features,
  };
}

/** Deliberate offline maintenance shutdown. Live-channel policy belongs to the
 * caller; automatic survivor replacement must use the empty-only operation. */
export async function shutdownKeeperAuthenticated(
  endpoint: LocalEndpoint,
  timeoutMs: number = 2_000,
): Promise<boolean> {
  return requestKeeperShutdown(endpoint, timeoutMs);
}

/** Identity-fenced automatic replacement. Emptiness is checked in this fresh
 * Hello and atomically again by the keeper when it dispatches the request. */
export async function shutdownEmptyKeeperAuthenticated(
  endpoint: LocalEndpoint,
  expected: EmptyKeeperShutdownExpectation,
  timeoutMs: number = 2_000,
): Promise<boolean> {
  return requestKeeperShutdown(endpoint, timeoutMs, expected);
}

async function requestKeeperShutdown(
  endpoint: LocalEndpoint,
  timeoutMs: number,
  expected?: EmptyKeeperShutdownExpectation,
): Promise<boolean> {
  const attempt = await connectKeeperAuthenticated(endpoint, timeoutMs);
  if (!attempt.authenticated) return false;
  const socket = attempt.socket;
  if (expected) {
    const bindingDigest = createHash("sha256").update(keeperBindingDigestInput(
      attempt.bindings ?? [],
      attempt.spawningChannels ?? [],
    )).digest("hex");
    if (attempt.keeperPid !== expected.keeperPid
      || attempt.processEpoch !== expected.processEpoch
      || bindingDigest !== expected.bindingDigest
      || attempt.bindings?.length !== 0
      || attempt.spawningChannels?.length !== 0) {
      try { socket.destroy(); } catch { /* already closed */ }
      return false;
    }
  }
  const requestType = expected
    ? MuxFrameType.ShutdownIfEmpty
    : MuxFrameType.Shutdown;
  const acknowledgementType = expected
    ? MuxFrameType.ShutdownIfEmptyAck
    : MuxFrameType.ShutdownAck;
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
        if (
          frame.channelId !== 0
          || !isEmptyKeeperPayload(frame.payload)
        ) continue;
        if (frame.type === acknowledgementType) {
          finish(true);
          return;
        }
        if (frame.type === MuxFrameType.ShutdownIfEmptyReject) {
          finish(false);
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
      socket.write(encodeMuxFrame(requestType, 0, new Uint8Array(0)));
      socket.resume();
    } catch {
      finish(false);
    }
  });
}

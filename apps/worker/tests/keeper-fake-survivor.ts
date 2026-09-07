// Scriptable keeper survivor for boot-admission tests: a plain socket server
// that answers the authenticated Hello with whatever proof the case needs, from
// a full protocol-3 contract down to a protocol-2 keeper that predates binding
// proof. Callers pass their own endpoint so each suite keeps its own data dir.

import { createServer, type Server, type Socket } from "node:net";
import type { LocalEndpoint } from "@roost/shared/local-endpoint";
import { prepareLocalEndpoint } from "@roost/shared/local-endpoint";
import type { KeeperContractV1 } from "@roost/shared/keeper-update";
import { KEEPER_TARGET_CONTRACT } from "../src/keeper/keeper-stamp.ts";
import {
  KEEPER_PROTOCOL_VERSION,
  MuxFrameType,
  decodeMuxFrames,
  encodeMuxFrame,
} from "../src/keeper/protocol.ts";

export const FAKE_KEEPER_PID = 424242;
export const FAKE_KEEPER_EPOCH = "50000000-0000-4000-8000-000000000005";

export interface FakeKeeperOptions {
  /** Milliseconds between receiving Hello and answering it. */
  helloDelayMs?: number;
  /** Never answer Hello: the endpoint accepts but proves nothing. */
  silent?: boolean;
  /** A field this build has never heard of, as a newer keeper would send. */
  unknownField?: boolean;
  contract?: KeeperContractV1;
  bindings?: readonly { channel_id: number; pid: number }[];
  /** Authenticate as a protocol-2 keeper: pid and epoch, but no contract,
   * bindings, or spawning channels. That is the exact Hello a worker update
   * finds when the surviving keeper predates the contract/binding proof. */
  preContract?: boolean;
  /** Accept the unfenced Shutdown request and stop listening, as a survivor
   * must for the operator-authorized force-live retirement to complete. */
  retiresOnShutdown?: boolean;
}

const servers: Server[] = [];
const openSockets = new Set<Socket>();

// The delays under test are real socket latency: the retry loop reads the wall
// clock and reacts to kernel socket events, which fake timers cannot drive.
export async function startFakeKeeper(
  endpoint: LocalEndpoint,
  options: FakeKeeperOptions,
): Promise<Server> {
  await prepareLocalEndpoint(endpoint);
  const server = createServer((socket) => {
    openSockets.add(socket);
    socket.on("close", () => openSockets.delete(socket));
    socket.on("error", () => { /* client hangs up on probe timeout */ });
    let received = Buffer.alloc(0) as Buffer;
    socket.on("data", (chunk: Buffer) => {
      received = Buffer.concat([received, Buffer.from(chunk)]);
      const { frames, remaining } = decodeMuxFrames(received);
      received = remaining;
      for (const frame of frames) {
        if (options.retiresOnShutdown && frame.type === MuxFrameType.Shutdown) {
          socket.write(
            encodeMuxFrame(MuxFrameType.ShutdownAck, 0, new Uint8Array(0)),
            () => {
              server.close();
              for (const open of openSockets) open.destroy();
              openSockets.clear();
            },
          );
          continue;
        }
        if (frame.type !== MuxFrameType.Hello || options.silent) continue;
        setTimeout(() => {
          if (socket.destroyed) return;
          socket.write(encodeMuxFrame(
            MuxFrameType.HelloResp,
            0,
            helloResponseBytes(options),
          ));
        }, options.helloDelayMs ?? 0);
      }
    });
  });
  servers.push(server);
  const listening = Promise.withResolvers<void>();
  server.once("error", listening.reject);
  server.listen(endpoint.address, () => listening.resolve());
  await listening.promise;
  return server;
}

/** Drop every live client socket, then close every server this module started. */
export async function stopFakeKeepers(): Promise<void> {
  for (const socket of openSockets) socket.destroy();
  openSockets.clear();
  for (const server of servers) {
    const closed = Promise.withResolvers<void>();
    server.close(() => closed.resolve());
    await closed.promise;
  }
  servers.length = 0;
}

export function incompatibleKeeperContract(): KeeperContractV1 {
  return {
    ...KEEPER_TARGET_CONTRACT,
    required_features: [
      ...KEEPER_TARGET_CONTRACT.required_features,
      "keeper_requirement_this_worker_lacks_v1",
    ].sort(),
    build_sha: "incompatible-identity-fixture",
  };
}

function helloResponseBytes(options: FakeKeeperOptions): Uint8Array {
  if (options.preContract) {
    return Buffer.from(JSON.stringify({
      version: KEEPER_PROTOCOL_VERSION - 1,
      authenticated: true,
      features: [...KEEPER_TARGET_CONTRACT.supported_features],
      build: "pre-contract-identity-fixture",
      pid: FAKE_KEEPER_PID,
      process_epoch: FAKE_KEEPER_EPOCH,
    }), "utf8");
  }
  const response: Record<string, unknown> = {
    version: KEEPER_PROTOCOL_VERSION,
    authenticated: true,
    features: [...KEEPER_TARGET_CONTRACT.supported_features],
    contract: options.contract ?? KEEPER_TARGET_CONTRACT,
    pid: FAKE_KEEPER_PID,
    process_epoch: FAKE_KEEPER_EPOCH,
    bindings: options.bindings ?? [],
    spawning_channels: [],
  };
  if (options.unknownField) {
    response.keeper_future_capability = { generation: 2, notes: "unknown" };
  }
  return Buffer.from(JSON.stringify(response), "utf8");
}

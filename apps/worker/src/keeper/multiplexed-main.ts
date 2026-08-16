// Multiplexed keeper subprocess. One process per worker; hosts N PTYs over a
// capability-authenticated local endpoint. Channel_id discriminates frames.
//
// Entry for `bun run multiplexed-main.ts <endpoint>` (source), compiled
// self-exec `roost keeper <endpoint>`, and supervised
// `roost keeper --service`. The body is side-effect-free on import.

import * as fs from "node:fs";
import * as net from "node:net";
import {
  LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS,
  LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES,
  LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS,
  cleanupLocalEndpoint,
  localEndpointFromEnv,
  prepareLocalEndpoint,
  secureLocalEndpoint,
  verifyLocalEndpointCapability,
  type LocalEndpoint,
} from "@roost/shared";
import {
  KEEPER_PROTOCOL_VERSION,
  MuxFrameType,
  decodeKeeperHelloRequest,
  decodeMuxFrames,
  encodeKeeperHelloResponse,
  encodeMuxFrame,
  isEmptyKeeperPayload,
  negotiateKeeperFeatures,
  type MuxFrame,
} from "./protocol-v2.ts";
import { KEEPER_BUILD_STAMP } from "./keeper-stamp.ts";
import { _log } from "./keeper-log.ts";
import { reapAllChannels } from "./keeper-process-reap.ts";
import { handleFrame, type FrameHandlerCtx } from "./keeper-frame-handler.ts";
import { muxLocalEndpoint } from "./keeper-pool-config.ts";
import type { Channel, ClientState } from "./keeper-types.ts";

interface KeeperClientState extends ClientState {
  authenticated: boolean;
  protocolCompatible: boolean;
  unauthenticatedBytes: number;
  authenticationTimer: NodeJS.Timeout | null;
}

function endpointForKeeper(argument: string): LocalEndpoint {
  if (argument === "--service") return muxLocalEndpoint();
  const hasSpawnHandoff = [
    "ROOST_KEEPER_ENDPOINT",
    "ROOST_KEEPER_CAPABILITY",
    "ROOST_KEEPER_ENDPOINT_KIND",
    "ROOST_KEEPER_CAPABILITY_PATH",
  ].some(name => process.env[name] !== undefined);
  const endpoint = hasSpawnHandoff
    ? localEndpointFromEnv(process.env, "ROOST_KEEPER")
    : muxLocalEndpoint();
  if (endpoint.address !== argument) {
    throw new Error("keeper endpoint argument does not match protected endpoint state");
  }
  return endpoint;
}

export function runKeeper(endpointArgument: string): void {
  void startKeeper(endpointArgument).catch((error) => {
    _log("error", "multiplexed-keeper", "startup_failed", { error: String(error) });
    process.exit(1);
  });
}

async function startKeeper(endpointArgument: string): Promise<void> {
  const endpoint = endpointForKeeper(endpointArgument);
  await prepareLocalEndpoint(endpoint);

  const pidPath = endpoint.isFilesystemPath ? `${endpoint.address}.pid` : null;
  let ownsPidFile = false;
  const removePidFile = () => {
    if (!pidPath || !ownsPidFile) return;
    try { fs.unlinkSync(pidPath); } catch { /* already absent */ }
  };
  process.once("exit", removePidFile);

  const channels = new Map<number, Channel>();
  // Only authenticated clients enter this set: broadcast must never leak PTY
  // output to a peer that merely connected to the local transport.
  const clients = new Set<KeeperClientState>();
  const unauthenticatedClients = new Set<KeeperClientState>();
  let shuttingDown = false;

  function broadcast(frame: Buffer): void {
    for (const client of clients) {
      try { client.socket.write(frame); } catch { /* dead socket */ }
    }
  }

  const frameCtx: FrameHandlerCtx = { channels, broadcast };

  function removeClient(client: KeeperClientState): void {
    if (client.authenticationTimer) {
      clearTimeout(client.authenticationTimer);
      client.authenticationTimer = null;
    }
    unauthenticatedClients.delete(client);
    clients.delete(client);
  }

  function rejectClient(client: KeeperClientState): void {
    removeClient(client);
    try { client.socket.destroy(); } catch { /* already closed */ }
  }

  let server: net.Server;

  async function shutdown(reason: string, acknowledge?: net.Socket): Promise<void> {
    if (shuttingDown) return;
    shuttingDown = true;
    _log("info", "multiplexed-keeper", "shutting_down", {
      reason,
      channels: channels.size,
    });
    if (acknowledge && !acknowledge.destroyed) {
      await new Promise<void>((resolve) => {
        let done = false;
        const finish = () => {
          if (done) return;
          done = true;
          clearTimeout(timer);
          resolve();
        };
        const timer = setTimeout(finish, 250);
        try {
          acknowledge.end(
            encodeMuxFrame(MuxFrameType.ShutdownAck, 0, new Uint8Array(0)),
            finish,
          );
        } catch {
          finish();
        }
      });
    }
    try { server.close(); } catch { /* not listening or already closed */ }
    for (const client of unauthenticatedClients) rejectClient(client);
    for (const client of clients) rejectClient(client);
    try {
      await reapAllChannels(channels);
    } catch (error) {
      _log("error", "multiplexed-keeper", "child_reap_failed", {
        error: String(error),
      });
    }
    removePidFile();
    try {
      await cleanupLocalEndpoint(endpoint);
    } catch (error) {
      _log("error", "multiplexed-keeper", "endpoint_cleanup_failed", {
        error: String(error),
        kind: endpoint.kind,
      });
    }
    process.exit(0);
  }

  server = net.createServer((socket) => {
    if (unauthenticatedClients.size >= LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS) {
      socket.destroy();
      return;
    }
    const client: KeeperClientState = {
      buf: Buffer.alloc(0),
      socket,
      authenticated: false,
      protocolCompatible: false,
      unauthenticatedBytes: 0,
      authenticationTimer: null,
    };
    unauthenticatedClients.add(client);
    client.authenticationTimer = setTimeout(
      () => rejectClient(client),
      LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS,
    );

    socket.on("data", (chunk: Buffer | Uint8Array) => {
      if (!client.authenticated) {
        client.unauthenticatedBytes += chunk.byteLength;
        if (client.unauthenticatedBytes > LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES) {
          rejectClient(client);
          return;
        }
      }
      client.buf = Buffer.concat([client.buf, Buffer.from(chunk)]);
      let frames: MuxFrame[];
      let remaining: Buffer;
      try {
        ({ frames, remaining } = decodeMuxFrames(client.buf));
      } catch {
        rejectClient(client);
        return;
      }
      client.buf = remaining;

      for (const frame of frames) {
        if (!client.authenticated) {
          if (frame.type !== MuxFrameType.Hello || frame.channelId !== 0) {
            rejectClient(client);
            return;
          }
          const hello = decodeKeeperHelloRequest(frame.payload);
          if (!hello
              || !verifyLocalEndpointCapability(endpoint.capability, hello.capability)) {
            rejectClient(client);
            return;
          }
          const features = negotiateKeeperFeatures(hello.features);
          try {
            socket.write(encodeMuxFrame(
              MuxFrameType.HelloResp,
              0,
              encodeKeeperHelloResponse({
                version: KEEPER_PROTOCOL_VERSION,
                authenticated: true,
                features,
                build: KEEPER_BUILD_STAMP,
                pid: process.pid,
              }),
            ));
          } catch {
            rejectClient(client);
            return;
          }
          client.authenticated = true;
          client.protocolCompatible = hello.version === KEEPER_PROTOCOL_VERSION;
          clearTimeout(client.authenticationTimer ?? undefined);
          client.authenticationTimer = null;
          unauthenticatedClients.delete(client);
          clients.add(client);
          continue;
        }

        if (frame.type === MuxFrameType.Shutdown) {
          if (frame.channelId !== 0 || !isEmptyKeeperPayload(frame.payload)) {
            rejectClient(client);
            return;
          }
          void shutdown("authenticated_shutdown", socket);
          return;
        }
        if (!client.protocolCompatible) {
          rejectClient(client);
          return;
        }
        // Isolate each command: one malformed channel operation must not crash
        // the keeper that owns every live PTY.
        try {
          handleFrame(frameCtx, client, frame);
        } catch (error) {
          _log("error", "multiplexed-keeper", "handle_frame_failed", {
            type: frame.type,
            channelId: frame.channelId,
            error: String(error),
          });
        }
      }
    });
    socket.on("close", () => removeClient(client));
    socket.on("error", (error) => {
      _log("warn", "multiplexed-keeper", "client_socket_error", {
        error: String(error),
      });
      removeClient(client);
    });
  });

  server.on("error", (error) => {
    _log("error", "multiplexed-keeper", "server_error", {
      error: String(error),
      endpoint: endpoint.address,
      kind: endpoint.kind,
    });
    if (server.listening) void shutdown("server_error");
    else process.exit(1);
  });

  server.listen(endpoint.address, () => {
    void (async () => {
      await secureLocalEndpoint(endpoint);
      if (pidPath) {
        try {
          fs.writeFileSync(pidPath, `${process.pid}\n`, { mode: 0o600 });
          ownsPidFile = true;
        } catch (error) {
          _log("error", "multiplexed-keeper", "pid_file_write_failed", {
            error: String(error),
            pidPath,
            endpoint: endpoint.address,
          });
        }
      }
      _log("info", "multiplexed-keeper", "listening", {
        endpoint: endpoint.address,
        kind: endpoint.kind,
      });
    })().catch((error) => {
      _log("error", "multiplexed-keeper", "endpoint_secure_failed", {
        error: String(error),
        kind: endpoint.kind,
      });
      void shutdown("endpoint_secure_failed");
    });
  });

  process.on("uncaughtException", (error) => {
    _log("error", "multiplexed-keeper", "uncaught_exception", {
      error: String(error),
      stack: error?.stack ?? null,
    });
  });
  process.on("unhandledRejection", (reason) => {
    _log("error", "multiplexed-keeper", "unhandled_rejection", {
      reason: String(reason),
    });
  });
  process.on("SIGTERM", () => {
    void shutdown("sigterm");
  });

  if (endpoint.isFilesystemPath) {
    setInterval(() => {
      if (!fs.existsSync(endpoint.address)) {
        void shutdown("socket_removed");
      }
    }, 30_000);
  }
}

if (import.meta.main) {
  const endpointArgument = process.argv[2];
  if (!endpointArgument) {
    _log("error", "multiplexed-keeper", "missing_endpoint_arg");
    process.exit(2);
  }
  runKeeper(endpointArgument);
}

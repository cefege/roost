// Worker → coord Connect client. Replaces tRPC after crpc6 cleanup.
// Used by: heartbeat.ts (register+heartbeat), install.ts (redeemWorker).
// SessionEvent emission rides CoordLink bidi (not this client).

import { createClient } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-node";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import type { WorkerConfig as WorkerConfigType } from "@roost/shared";
import { log } from "@roost/shared";

export type CoordClientOptions = {
  cfg: WorkerConfigType;
  getJwt: () => Promise<string>;
};

export function createCoordClient(opts: CoordClientOptions) {
  const { cfg, getJwt } = opts;
  const transport = createConnectTransport({
    baseUrl: cfg.coordinatorUrl,
    httpVersion: "1.1",
    useBinaryFormat: false,
    interceptors: [
      (next) => async (req) => {
        try {
          const jwt = await getJwt();
          req.header.set("authorization", `Bearer ${jwt}`);
        } catch (e) {
          log.warn("coord-client", "jwt mint failed", { error: String(e) });
        }
        return next(req);
      },
    ],
  });
  return createClient(CoordinatorService, transport);
}

export function createUnauthenticatedCoordClient(baseUrl: string) {
  return createClient(CoordinatorService, createConnectTransport({
    baseUrl,
    httpVersion: "1.1",
    useBinaryFormat: false,
  }));
}

export type CoordClient = ReturnType<typeof createCoordClient>;

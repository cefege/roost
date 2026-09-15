// Loopback origins for the terminal smoke stack's worker-served local UI.
// Every worker this harness spawns gets its own port instead of the shared
// 127.0.0.1:4104 default: one stack runs several workers, several stacks run
// concurrently, and the worker's local UI server throws EADDRINUSE instead of
// falling back. A reservation keeps its listening socket until the worker is
// about to bind, and the coordinator's origin allowlist is built from them all.

import { createServer, type Server } from "node:net";

const LOOPBACK_HOST = "127.0.0.1";

export interface WorkerLocalUi {
  /** Value the stack hands the child as ROOST_WORKER_LOCAL_UI_BIND. */
  readonly bind: string;
  /** Origin that worker serves; the coordinator must allowlist it. */
  readonly origin: string;
  /** Drop the harness's hold, immediately before the worker binds the port. */
  release(): Promise<void>;
  /** Publish this worker's origin once its fingerprint is known. */
  record(workerFp: string): void;
}

export interface LocalUiOrigins {
  /** Reserve and hold an OS-assigned loopback port for one worker. */
  reserve(label: string): Promise<WorkerLocalUi>;
  /** Every origin reserved so far, for the coordinator's CORS and WS gates. */
  origins(): string[];
  /** Origin a recorded worker serves the SPA and the local terminal socket on. */
  url(workerFp: string): string;
  /** Release holds no worker ever took over, on stack teardown. */
  closeAll(): Promise<void>;
}

export function createLocalUiOrigins(): LocalUiOrigins {
  const originByFingerprint = new Map<string, string>();
  const reservedOrigins: string[] = [];
  const holds = new Set<Server>();
  return {
    reserve: async (label) => {
      const { server, port } = await holdLoopbackPort(label);
      const bind = `${LOOPBACK_HOST}:${port}`;
      const origin = `http://${bind}`;
      holds.add(server);
      reservedOrigins.push(origin);
      return {
        bind,
        origin,
        release: async () => {
          if (!holds.delete(server)) return;
          await new Promise<void>((resolve) => { server.close(() => resolve()); });
        },
        record: (workerFp) => { originByFingerprint.set(workerFp, origin); },
      };
    },
    origins: () => [...reservedOrigins],
    url: (workerFp) => {
      const origin = originByFingerprint.get(workerFp);
      if (origin === undefined) {
        const known = [...originByFingerprint.keys()].join(", ");
        throw new Error(
          `no local UI origin for worker ${workerFp}; this stack started: ${known || "none"}`,
        );
      }
      return origin;
    },
    closeAll: async () => {
      for (const server of [...holds]) {
        holds.delete(server);
        await new Promise<void>((resolve) => { server.close(() => resolve()); });
      }
    },
  };
}

/** Hold a free loopback port so nothing else can take it before the child binds. */
async function holdLoopbackPort(label: string): Promise<{ server: Server; port: number }> {
  const server = createServer();
  const port = await new Promise<number>((resolve, reject) => {
    server.once("error", reject);
    server.listen({ host: LOOPBACK_HOST, port: 0 }, () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        reject(new Error(`local UI port reservation for ${label} returned no address`));
        return;
      }
      resolve(address.port);
    });
  });
  return { server, port };
}

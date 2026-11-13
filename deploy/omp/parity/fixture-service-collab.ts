import type { BuiltinSeedContext } from "./builtin-seed";
import type { LocalHttpService, ServiceTrace } from "./fixture-service-http";

export type CollabRelayServiceInput =
  | { mode: "canonical" }
  | { mode: "refusal"; closeCode: number; reason: string };

export async function startCanonicalCollabRelay(context: BuiltinSeedContext, trace: ServiceTrace): Promise<LocalHttpService> {
  const production = await context.load<typeof import("@omp-source/packages/collab-web/scripts/local-relay")>("packages/collab-web/scripts/local-relay.ts");
  const relay = production.startLocalRelay(0, { hostname: "127.0.0.1" });
  try {
    if (relay.hostname !== "127.0.0.1" || relay.url !== `ws://127.0.0.1:${relay.port}` || !Number.isInteger(relay.port) || relay.port < 1) throw new Error("Canonical collaboration helper did not bind owned loopback");
    trace("collab-relay", { direction: "listening", transport: "websocket", origin: relay.url, port: relay.port, source: "packages/collab-web/scripts/local-relay.ts" });
  } catch (error) {
    await relay.stop();
    throw error;
  }
  return { url: relay.url, port: relay.port, dispose: () => relay.stop() };
}

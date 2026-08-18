import type { ServiceHealthRole, ServiceHealthStatusFor } from "@roost/shared/service-health";

/** `ServiceHealthProver.read` and `WindowsLocalEndpointHealth.read` are generic
 *  over the role requested: the caller narrows the descriptor by the role it
 *  asks for. A double must honour that rather than pin one concrete role, so
 *  build it from a per-role table and let unprobed roles throw. */
export type RoleHealthTable = { [R in ServiceHealthRole]: () => ServiceHealthStatusFor<R> };

export function readHealthByRole(
  table: RoleHealthTable,
): <R extends ServiceHealthRole>(role: R) => Promise<ServiceHealthStatusFor<R>> {
  return async <R extends ServiceHealthRole>(role: R) => table[role]();
}

export const FIXTURE_COORDINATOR_URL = "https://coordinator.example.test";

/** The update broker's pre-update checkpoint keeps only role/version/build/
 *  processEpoch plus the worker's coordinatorUrl, so the readiness flags here
 *  complete the health contract without reaching any assertion. */
export function fixtureHealthTable(
  version = "1.0.0",
  build = "1".repeat(40),
): RoleHealthTable {
  const common = { version, build, ready: true };
  return {
    worker: () => ({
      ...common,
      role: "worker",
      processEpoch: "worker-epoch-1",
      targetLinkReady: true,
      coordinatorUrl: FIXTURE_COORDINATOR_URL,
    }),
    coordinator: () => ({
      ...common,
      role: "coordinator",
      processEpoch: "coordinator-epoch-1",
      dbReady: true,
      listenerReady: true,
    }),
  };
}

/** Bun's `typeof fetch` carries `preconnect`, so a bare arrow is not a complete
 *  stand-in for an injected fetch seam. */
export function stubFetch(body: (url: string) => string): typeof fetch {
  return Object.assign(
    async (input: Parameters<typeof fetch>[0]): Promise<Response> =>
      new Response(body(String(input)), { status: 200 }),
    { preconnect: fetch.preconnect },
  );
}

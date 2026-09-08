// `roost worker` — run the worker in THIS process (the compiled binary's worker
// mode). Dials the coordinator (ROOST_COORDINATOR_URL), owns local PTYs via the
// keeper, and relays local OMP bridge state. Same entry the
// LaunchAgent uses; from source it's `bun run apps/worker/src/main.ts`.

export async function worker(_args: string[]): Promise<void> {
  // A static import cannot work here: worker config is module-scoped and this
  // module is reachable from every CLI command, so loading the worker runtime
  // eagerly would read config for commands that never run one.
  const { runWorker } = await import("../../worker/src/main.ts");
  await runWorker();
}

// `roost worker` — run the worker in THIS process (the compiled binary's worker
// mode). Dials the coordinator (ROOST_COORDINATOR_URL), owns local PTYs via the
// keeper, and relays local OMP bridge state. Same entry the
// LaunchAgent uses; from source it's `bun run apps/worker/src/main.ts`.
import { runWorker } from "../../worker/src/main.ts";

export async function worker(_args: string[]): Promise<void> {
  await runWorker();
}

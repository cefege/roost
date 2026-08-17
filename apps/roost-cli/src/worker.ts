// `roost worker` — run the worker in THIS process (the compiled binary's worker
// mode). Dials the coordinator (ROOST_COORDINATOR_URL), owns local PTYs via the
// keeper, and relays local OMP bridge state. Same entry the
// LaunchAgent uses; from source it's `bun run apps/worker/src/main.ts`.
import { roostServiceDir } from "@roost/shared/paths";
import { readWindowsRelocationRoleOverride } from "@roost/shared/windows-relocation";

export async function worker(_args: string[]): Promise<void> {
  if (process.platform === "win32") {
    const override = readWindowsRelocationRoleOverride(roostServiceDir(), "worker");
    if (override) {
      for (const [key, value] of Object.entries(override.environment)) process.env[key] = value;
    }
  }
  // A static import cannot work here: Worker config is module-scoped and must
  // observe the updater-owned endpoint override.
  const { runWorker } = await import("../../worker/src/main.ts");
  await runWorker();
}

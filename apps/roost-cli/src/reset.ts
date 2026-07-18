// `roost reset` — nuke local state. Stops LaunchAgents, wipes new-coord
// DB + pinned keys, runs `bun install`. Legacy state untouched.

import { spawn } from "bun";
import { rmSync, existsSync } from "node:fs";

export async function reset(_args: string[]): Promise<void> {
  console.log(">> stop com.roost.coordinator + com.roost.worker");
  await spawn({
    cmd: ["bash", "-c", "launchctl bootout gui/$UID/com.roost.coordinator 2>/dev/null; launchctl bootout gui/$UID/com.roost.worker 2>/dev/null; true"],
    stdio: ["inherit", "inherit", "inherit"],
  }).exited;

  const home = process.env.HOME!;
  const wipe = [
    `${home}/Library/Application Support/RoostCoordinator/coordinator_v2.db`,
    `${home}/Library/Application Support/RoostCoordinator/coordinator_v2.db-shm`,
    `${home}/Library/Application Support/RoostCoordinator/coordinator_v2.db-wal`,
  ];
  for (const p of wipe) {
    if (existsSync(p)) {
      console.log(`>> rm ${p}`);
      rmSync(p);
    }
  }

  console.log(">> bun install");
  await spawn({ cmd: ["bun", "install"], stdio: ["inherit", "inherit", "inherit"] }).exited;
  console.log(">> done");
}

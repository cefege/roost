// `roost reset` — nuke local state. Stops the coord + worker services
// (launchd on macOS, systemd --user on Linux), wipes new-coord DB +
// pinned keys, runs `bun install`. Legacy state untouched.

import { spawn } from "bun";
import { rmSync, existsSync } from "node:fs";
import { join } from "node:path";
import { coordDataDir } from "@roost/shared/paths";
import { currentServiceOs, stopServicesCmd } from "./service-ctl.ts";

export async function reset(_args: string[]): Promise<void> {
  console.log(">> stop coord + worker services");
  await spawn({
    cmd: ["bash", "-c", stopServicesCmd(currentServiceOs())],
    stdio: ["inherit", "inherit", "inherit"],
  }).exited;

  // coordDataDir() honours ROOST_COORD_DATA_DIR, which the installer sets on
  // both platforms, so an isolated test install resets its own DB.
  const base = join(coordDataDir(), "coordinator_v2.db");
  const wipe = [base, `${base}-shm`, `${base}-wal`];
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

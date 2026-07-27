// `roost join` — run on a NEW machine (invoked by join.sh) to install + start
// the local worker and register it with the coordinator. The pull counterpart
// to `roost deploy`: no SSH, no rsync — the code is already cloned here, so
// this just runs the local-install path. Reads ROOST_COORDINATOR_URL +
// ROOST_BOOTSTRAP_TOKEN (+ optional ROOST_WORKER_LABEL) from the environment,
// which join.sh exports from the one-liner emitted by `roost add-mac`.

import { _deployLocal } from "./deploy-local.ts";

export async function join(_args: string[]): Promise<void> {
  if (!process.env.ROOST_COORDINATOR_URL) {
    console.error("ERROR: ROOST_COORDINATOR_URL required — get the join command from");
    console.error("  `roost add-mac` on your coordinator (or Settings → Machines → Add machine).");
    process.exit(1);
  }
  if (!process.env.ROOST_BOOTSTRAP_TOKEN) {
    console.error("ERROR: ROOST_BOOTSTRAP_TOKEN required — get the join command from");
    console.error("  `roost add-mac` on your coordinator (or Settings → Machines → Add machine).");
    process.exit(1);
  }

  await _deployLocal("this machine");

  console.log("");
  console.log("Joined. This machine should appear in Settings → Machines within a few seconds.");
  console.log("  check: bun apps/roost-cli/src/main.ts status");
}

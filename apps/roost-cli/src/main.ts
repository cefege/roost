#!/usr/bin/env bun
// roost CLI — single entry-point for dev, test, deploy, logs, reset,
// state, cutover. Replaces the 7+ scattered shell scripts.
// REWRITE.md R0.8.

import { dev } from "./dev.ts";
import { test } from "./test.ts";
import { deploy } from "./deploy.ts";
import { push } from "./push.ts";
import { keeperRefresh } from "./keeper-refresh.ts";
import { logs } from "./logs.ts";
import { reset } from "./reset.ts";
import { state } from "./state.ts";
import { cutover } from "./cutover.ts";
import { status } from "./status.ts";
import { quickstart } from "./quickstart.ts";
import { doctor } from "./doctor.ts";
import { api } from "./api.ts";
import { join } from "./join.ts";
import { addMac } from "./add-mac.ts";
import { coord } from "./coord.ts";
import { worker } from "./worker.ts";
import { keeper } from "./keeper.ts";
import { update } from "./update.ts";
import { version } from "./version.ts";

const SUBCOMMANDS = {
  quickstart,
  coord,
  worker,
  keeper,
  update,
  version,
  dev,
  test,
  deploy,
  push,
  "keeper-refresh": keeperRefresh,
  logs,
  reset,
  state,
  cutover,
  status,
  doctor,
  api,
  join,
  "add-mac": addMac,
} as const;

type Subcommand = keyof typeof SUBCOMMANDS;

function usage(): never {
  console.error("Usage: bun run roost <subcommand> [args]");
  console.error("Subcommands:");
  console.error("  quickstart        one-shot local install (tailscale → coord + worker + browser)");
  console.error("  coord             run the coordinator (server mode; used by the compiled binary)");
  console.error("  worker            run the worker (server-side; compiled binary / LaunchAgent)");
  console.error("  dev               start coord + worker + web dev servers");
  console.error("  test              run all tests in dep order");
  console.error("  deploy <host>     deploy worker to a tailnet host");
  console.error("  push              git push + deploy fleet + kickstart local coord");
  console.error("  keeper-refresh <host> --yes   re-spawn a host's keeper on current code (destructive)");
  console.error("  logs <app>        tail an app's logs (coord|worker) [--tail N]");
  console.error("  reset             nuke local state (DB, keys, lock)");
  console.error("  state             print STATE.md snapshot");
  console.error("  cutover           migrate from coordinator.db → coordinator_v2.db");
  console.error("  status            health readout (tailscale, agents, coord, workers)");
  console.error("  doctor [--since]  daily anomaly digest from err logs (default 24h)");
  console.error("  api <verb>        headless introspect/drive (sessions|cells|input|message|rename|assign|attach|spawn|kill|workers|workspaces|ws-*|tasks|task-*|ui|ui-state|agent|events)");
  console.error("  add-mac [--label X]  print a copy-paste command to add another Mac (run on the coordinator)");
  console.error("  join                install + register this Mac's worker (used by join.sh; needs ROOST_COORDINATOR_URL + ROOST_BOOTSTRAP_TOKEN)");
  console.error("  update            self-update the binary from the latest GitHub release");
  console.error("  version           print the roost version");
  process.exit(1);
}

const [, , sub, ...args] = process.argv;
const cmd = sub === "--version" || sub === "-v" ? "version" : sub;
if (!cmd || !(cmd in SUBCOMMANDS)) usage();
try {
  await SUBCOMMANDS[cmd as Subcommand](args);
} catch (error) {
  console.error(JSON.stringify({ cmd, error: String(error) }));
  process.exit(1);
}

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
import { addMachine } from "./add-machine.ts";
import { worker } from "./worker.ts";
import { keeper } from "./keeper.ts";
import { update } from "./update.ts";
import { version } from "./version.ts";
import { expose } from "./expose.ts";

const SUBCOMMANDS = {
  quickstart,
  coord: async (args: string[]) => {
    // Loading coordinator code imports the generated SPA embed. Keep that
    // command-only dependency out of `roost test`, which builds the web bundle.
    const { coord } = await import("./coord.ts");
    return coord(args);
  },
  worker,
  keeper,
  update,
  "__windows-updater-broker": async (args: string[]) => {
    if (process.platform !== "win32" || args.length !== 0) {
      throw new Error("internal Windows updater broker dispatch refused");
    }
    // Platform-only modules depend on Windows native helpers and must not load
    // into POSIX command paths.
    const [
      { createWindowsServiceManager },
      { runWindowsUpdateBroker },
      { DurableWindowsUpdateJournalStore },
      { createServiceHealthProver, createWindowsUpdateNative },
      { admitPendingWindowsUpdateRequest },
      { createWindowsRelocationBrokerDeps, runWindowsRelocationBroker },
      { admitPendingWindowsRelocationRequest },
    ] = await Promise.all([
      import("./service-ctl.ts"),
      import("./windows-update-broker.ts"),
      import("./windows-update-journal.ts"),
      import("./windows-update-runtime.ts"),
      import("./windows-update-control.ts"),
      import("./windows-relocation-broker.ts"),
      import("./windows-relocation-control.ts"),
    ]);
    for (let admitted = 0; admitted < 16; admitted += 1) {
      const journal = await admitPendingWindowsRelocationRequest();
      if (!journal) break;
      await runWindowsRelocationBroker(
        createWindowsRelocationBrokerDeps(journal.operationKind),
      );
    }
    let relocationHandled = false;
    for (const kind of ["worker-endpoint", "coordinator-promotion"] as const) {
      const relocation = await runWindowsRelocationBroker(
        createWindowsRelocationBrokerDeps(kind),
      );
      relocationHandled ||= relocation.handled;
    }
    if (relocationHandled) return;
    await admitPendingWindowsUpdateRequest();
    await runWindowsUpdateBroker({
      store: new DurableWindowsUpdateJournalStore(),
      services: createWindowsServiceManager(),
      native: createWindowsUpdateNative(),
      health: createServiceHealthProver(),
    });
  },
  version,
  expose,
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
  "add-machine": addMachine,
} as const;

type Subcommand = keyof typeof SUBCOMMANDS;

function usage(): never {
  console.error("Usage: bun run roost <subcommand> [args]");
  console.error("Subcommands:");
  console.error("  quickstart        one-shot local install (tailscale → coord + worker + browser)");
  console.error("  coord             run the coordinator (server mode; used by the compiled binary)");
  console.error("  worker            run the worker (server-side; compiled binary / LaunchAgent)");
  console.error("  expose <hostname> --team <team>.cloudflareaccess.com --aud <64-hex> [--config <path>]");
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
  console.error("  api <verb>        headless introspect/drive (sessions|cells|input|rename|assign|attach|spawn|kill|workers|workspaces|ws-*|tasks|task-*|ui|ui-state|events)");
  console.error("  add-machine --platform <macos|linux|windows> [--label X] [--publisher-sha256 HEX]  print a one-shot enrollment command");
  console.error("  join                install + register this machine's worker (used by join.sh; needs ROOST_COORDINATOR_URL + ROOST_BOOTSTRAP_TOKEN)");
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
  const exitCode = error && typeof error === "object" && "exitCode" in error
    && typeof error.exitCode === "number" ? error.exitCode : 1;
  process.exit(exitCode);
}

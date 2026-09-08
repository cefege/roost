// `roost quickstart` — the local one-shot installer for the one supported
// shape: the coordinator listens plaintext on loopback and the operator's own
// front door serves `--coordinator-url`. Endpoint validation is deliberately
// pure/read-only and runs before any install, state, credential, or service
// mutation. Calls into quickstart-endpoint/-runtime/-windows-install.

import * as nodeFs from "node:fs";
import { basename } from "node:path";
import { deploy } from "./deploy.ts";
import {
  installCoordAgent,
  installWorkerAgent,
  readWindowsServiceCredentials,
} from "./install-binary-agents.ts";
import {
  mintBrowserToken,
  mintWorkerToken,
} from "./quickstart-bootstrap-tokens.ts";
import {
  coordinatorEnvironmentForQuickstart,
  resolveQuickstartEndpoint,
} from "./quickstart-endpoint.ts";
import {
  die,
  dryRunServiceDefinitions,
  installRoostShim,
  logStep,
  openQuickstartBrowser,
  runInherit,
  waitForCoordHealth,
  waitForWorkerRegistration,
} from "./quickstart-runtime.ts";
import {
  beginWindowsQuickstartInstall,
  commitWindowsQuickstartInstall,
  prepareWindowsQuickstartCoordinatorState,
  proveWindowsInstallHealth,
  rollbackWindowsQuickstartInstall,
} from "./quickstart-windows-install.ts";
import type { WindowsQuickstartInstall } from "./quickstart-windows-install.ts";
import { coordinatorPaths } from "./quickstart-windows-state.ts";
import type { CoordinatorPaths } from "./quickstart-windows-state.ts";
import { printStatusReport, statusReport } from "./status.ts";
import { ROOST_VERSION } from "./version.ts";

const WEB_DIST_INDEX = "apps/web/dist/index.html";

export {
  coordinatorEnvironmentForQuickstart,
  resolveQuickstartEndpoint,
} from "./quickstart-endpoint.ts";
export type { QuickstartEndpoint } from "./quickstart-endpoint.ts";
export {
  openQuickstartBrowser,
  waitForCoordHealth,
} from "./quickstart-runtime.ts";
export type {
  QuickstartBrowserLauncher,
  QuickstartHealthDeps,
} from "./quickstart-runtime.ts";

export async function quickstart(args: string[]): Promise<void> {
  // The no-effect boundary. It precedes Windows credential reads, machine
  // transactions, state preparation, dependency installs, builds, and every
  // service definition write.
  const endpoint = resolveQuickstartEndpoint(args, process.env, process.platform);

  const force = args.includes("--force");
  const dry = args.includes("--dry-run");
  const binaryName = basename(process.execPath).toLocaleLowerCase("en-US");
  const binary = binaryName !== "bun" && binaryName !== "bun.exe";
  if (process.platform === "win32" && !binary) {
    die("Windows quickstart requires the signed compiled release", "run install-binary.ps1");
  }
  const publicUrl = endpoint.origin;
  logStep(`front door ${publicUrl} → coordinator 127.0.0.1:${endpoint.loopbackPort}`);

  const serviceCredentials = process.platform === "win32" && !dry
    ? args.includes("--windows-service-credential-stdin")
      ? await readWindowsServiceCredentials()
      : die(
        "Windows service credential frame is required",
        "run quickstart through the signed install-binary.ps1 front door",
      )
    : undefined;
  let windowsInstall: WindowsQuickstartInstall | null = null;
  let windowsPaths: CoordinatorPaths | null = null;
  let workerToken: string | undefined;
  let browserToken: string | undefined;

  try {
    windowsPaths = process.platform === "win32" ? coordinatorPaths() : null;
    if (process.platform === "win32" && binary && !dry && serviceCredentials && windowsPaths) {
      windowsInstall = await beginWindowsQuickstartInstall();
      await prepareWindowsQuickstartCoordinatorState(
        windowsInstall,
        windowsPaths,
        serviceCredentials.account,
      );
    }

    const endpointEnvironment = coordinatorEnvironmentForQuickstart(endpoint);
    const windowsCoordinatorEnvironment = windowsPaths
      ? {
        ROOST_COORD_DATA_DIR: windowsPaths.dataDir,
        ROOST_COORD_LOG_DIR: windowsPaths.logDir,
        ROOST_COORDINATOR_DB: windowsPaths.database,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: windowsPaths.authorizedKeys,
        ROOST_COORDINATOR_KEY_PATH: windowsPaths.key,
        ROOST_COORDINATOR_HANDOFF_PATH: windowsPaths.handoff,
        ...endpointEnvironment,
      }
      : undefined;
    const databasePath = windowsPaths?.database ?? coordinatorPaths().database;

    if (binary) {
      console.log(`   roost: ${process.execPath} (${ROOST_VERSION})`);
      await installCoordAgent({
        execPath: process.execPath,
        gitSha: ROOST_VERSION,
        cmd: dry ? "write-plist" : "install",
        credentials: serviceCredentials,
        env: windowsCoordinatorEnvironment ?? endpointEnvironment,
        log: logStep,
      });
      if (dry) {
        await installWorkerAgent({
          execPath: process.execPath,
          coordUrl: publicUrl,
          gitSha: ROOST_VERSION,
          cmd: "write-plist",
          coordinatorHost: true,
          coordinatorEnvironment: windowsCoordinatorEnvironment,
          log: logStep,
        });
        console.log("\n✓ --dry-run complete (service definitions generated; nothing installed).");
        return;
      }

      logStep("waiting for coordinator health");
      if (!await waitForCoordHealth(endpoint)) {
        die("coord did not become healthy on its loopback bind", "check logs: roost logs coord");
      }
      console.log(`   coord healthy on 127.0.0.1:${endpoint.loopbackPort}`);

      workerToken = await mintWorkerToken(databasePath, "quickstart-local-worker");
      await installWorkerAgent({
        execPath: process.execPath,
        coordUrl: publicUrl,
        bootstrapToken: workerToken,
        gitSha: ROOST_VERSION,
        cmd: "install",
        coordinatorHost: true,
        coordinatorEnvironment: windowsCoordinatorEnvironment,
        credentials: serviceCredentials,
        log: logStep,
      });
      if (windowsInstall && serviceCredentials) {
        await proveWindowsInstallHealth(
          windowsInstall,
          serviceCredentials.account,
          publicUrl,
        );
      }
    } else {
      console.log(`   bun: ${process.execPath}`);
      if (dry) {
        await dryRunServiceDefinitions(endpoint);
        console.log("\n✓ --dry-run complete (service definitions generated; nothing installed).");
        return;
      }
      if (force || !nodeFs.existsSync("node_modules")) {
        logStep("bun install");
        if (await runInherit([process.execPath, "install"]) !== 0) die("bun install failed");
      } else {
        logStep("bun install (skipped — node_modules present; --force to reinstall)");
      }
      if (force || !nodeFs.existsSync(WEB_DIST_INDEX)) {
        logStep("building web SPA (apps/web → dist)");
        if (await runInherit([process.execPath, "x", "vite", "build"], "apps/web") !== 0) {
          die("vite build failed");
        }
      } else {
        logStep("web SPA build (skipped — dist present)");
      }

      logStep("installing coordinator service");
      if (
        await runInherit(
          ["bash", "apps/coord/scripts/install.sh", "install"],
          undefined,
          endpointEnvironment,
        ) !== 0
      ) {
        die("coord install.sh failed");
      }
      logStep("waiting for coordinator health");
      if (!await waitForCoordHealth(endpoint)) {
        die("coord did not become healthy on its loopback bind", "check logs: roost logs coord");
      }
      console.log(`   coord healthy on 127.0.0.1:${endpoint.loopbackPort}`);

      workerToken = await mintWorkerToken(databasePath, "quickstart-local-worker");
      logStep("deploying local worker");
      const priorCoordinatorUrl = process.env.ROOST_COORDINATOR_URL;
      const priorBootstrapToken = process.env.ROOST_BOOTSTRAP_TOKEN;
      const priorAllowDirty = process.env.ROOST_ALLOW_DIRTY;
      try {
        process.env.ROOST_COORDINATOR_URL = publicUrl;
        process.env.ROOST_BOOTSTRAP_TOKEN = workerToken;
        process.env.ROOST_ALLOW_DIRTY = "1";
        await deploy(
          ["localhost", "--allow-unpublished-local"],
          { coordinatorUrl: publicUrl },
        );
      } finally {
        if (priorCoordinatorUrl === undefined) delete process.env.ROOST_COORDINATOR_URL;
        else process.env.ROOST_COORDINATOR_URL = priorCoordinatorUrl;
        if (priorBootstrapToken === undefined) delete process.env.ROOST_BOOTSTRAP_TOKEN;
        else process.env.ROOST_BOOTSTRAP_TOKEN = priorBootstrapToken;
        if (priorAllowDirty === undefined) delete process.env.ROOST_ALLOW_DIRTY;
        else process.env.ROOST_ALLOW_DIRTY = priorAllowDirty;
      }
    }

    if (!workerToken) throw new Error("quickstart worker grant was not minted");
    // The local worker dials the declared front door, so its registration is
    // also the proof that the operator's front door passes worker traffic.
    logStep("proving local worker registration");
    const workerFingerprint = await waitForWorkerRegistration(databasePath, workerToken);
    if (!workerFingerprint) {
      die(
        `local worker did not register through ${publicUrl}`,
        "check that your front door proxies to the coordinator's loopback bind, then: roost logs worker",
      );
    }
    workerToken = undefined;
    console.log(`   worker registered (${workerFingerprint.slice(0, 12)})`);

    const report = await statusReport({ origin: publicUrl });
    printStatusReport(report);
    if (
      windowsInstall
      && (!report.coordAgentLoaded
        || !report.workerAgentLoaded
        || !report.coord.reachable)
    ) {
      throw new Error("Windows quickstart status proof did not confirm all required services");
    }

    browserToken = await mintBrowserToken(databasePath, "quickstart-browser");
    logStep("opening the app with a one-shot browser grant");
    try {
      await openQuickstartBrowser(endpoint, browserToken, process.platform);
    } finally {
      browserToken = undefined;
    }

    const shim = binary ? null : installRoostShim(process.cwd());
    console.log("\n✓ Roost is running.");
    console.log(`  This machine:    ${publicUrl}`);
    console.log(`  Pair your phone: open ${publicUrl} → Settings → Pair a device → scan the QR`);
    if (binary || (shim && shim.onPath)) {
      console.log("  Health anytime:  roost status");
    } else if (shim) {
      console.log(`  Health anytime:  ${shim.path} status   (add ~/.bun/bin to PATH for bare \`roost\`)`);
    } else {
      console.log("  Health anytime:  bun apps/roost-cli/src/main.ts status");
    }
    if (windowsInstall) {
      await commitWindowsQuickstartInstall(windowsInstall);
    }
  } catch (error) {
    if (windowsInstall) {
      await rollbackWindowsQuickstartInstall(windowsInstall, error);
    }
    throw error;
  } finally {
    workerToken = undefined;
    browserToken = undefined;
    await windowsInstall?.lock.release();
    if (serviceCredentials) serviceCredentials.password = undefined;
  }
}

// `roost quickstart` selects one validated local-first endpoint profile, then
// installs a fresh pair or reconciles an existing coordinator without replacing
// its worker identity. Source and compiled paths share the same proof ordering.

import * as nodeFs from "node:fs";
import { basename } from "node:path";
import { deploy } from "./deploy.ts";
import {
  installCoordAgent,
  installWorkerAgent,
  readWindowsServiceCredentials,
} from "./install-binary-agents.ts";
import {
  discoverExistingQuickstartInstall,
  runExistingQuickstart,
} from "./quickstart-existing-install.ts";
import { mintBrowserToken, mintWorkerToken } from "./quickstart-bootstrap-tokens.ts";
import {
  coordinatorEnvironmentForQuickstart,
  parseQuickstartOptions,
  quickstartLoopbackOrigin,
  resolveQuickstartEndpoint,
} from "./quickstart-endpoint.ts";
import type { QuickstartEndpoint } from "./quickstart-endpoint.ts";
import {
  die,
  dryRunServiceDefinitions,
  installRoostShim,
  logStep,
  openQuickstartBrowser,
  runInherit,
  waitForCoordHealth,
  waitForCoordSpa,
  waitForWorkerRegistration,
  waitForWorkerRoutability,
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
  parseQuickstartOptions,
  quickstartLoopbackOrigin,
  resolveQuickstartEndpoint,
} from "./quickstart-endpoint.ts";
export type { QuickstartEndpoint, QuickstartOptions } from "./quickstart-endpoint.ts";
export {
  openQuickstartBrowser,
  waitForCoordHealth,
  waitForCoordSpa,
  waitForWorkerRoutability,
} from "./quickstart-runtime.ts";
export type {
  QuickstartBrowserLauncher,
  QuickstartHealthDeps,
} from "./quickstart-runtime.ts";

function sourceWorkerEnvironment(
  coordinatorUrl: string,
  bootstrapToken: string,
): () => void {
  const priorCoordinatorUrl = process.env.ROOST_COORDINATOR_URL;
  const priorBootstrapToken = process.env.ROOST_BOOTSTRAP_TOKEN;
  const priorAllowDirty = process.env.ROOST_ALLOW_DIRTY;
  process.env.ROOST_COORDINATOR_URL = coordinatorUrl;
  process.env.ROOST_BOOTSTRAP_TOKEN = bootstrapToken;
  process.env.ROOST_ALLOW_DIRTY = "1";
  return () => {
    if (priorCoordinatorUrl === undefined) delete process.env.ROOST_COORDINATOR_URL;
    else process.env.ROOST_COORDINATOR_URL = priorCoordinatorUrl;
    if (priorBootstrapToken === undefined) delete process.env.ROOST_BOOTSTRAP_TOKEN;
    else process.env.ROOST_BOOTSTRAP_TOKEN = priorBootstrapToken;
    if (priorAllowDirty === undefined) delete process.env.ROOST_ALLOW_DIRTY;
    else process.env.ROOST_ALLOW_DIRTY = priorAllowDirty;
  };
}

async function provisionSourceWorker(coordinatorUrl: string, bootstrapToken: string): Promise<void> {
  const restoreEnvironment = sourceWorkerEnvironment(coordinatorUrl, bootstrapToken);
  try {
    await deploy(["localhost", "--allow-unpublished-local"], { coordinatorUrl });
  } finally {
    restoreEnvironment();
  }
}

function printCompletion(options: {
  endpoint: QuickstartEndpoint;
  remoteAccessVerified: boolean | null;
  binary: boolean;
  shim: { path: string; onPath: boolean } | null;
}): void {
  console.log("\n✓ Roost is running.");
  console.log(`  Local access:    ${quickstartLoopbackOrigin(options.endpoint)}`);
  if (options.endpoint.webPublicUrl) {
    console.log(`  Remote access:   ${options.endpoint.webPublicUrl}${options.remoteAccessVerified === false ? " (remote access not verified)" : ""}`);
  } else {
    console.log("  Remote access:   optional / unconfigured");
  }
  if (options.binary || (options.shim && options.shim.onPath)) {
    console.log("  Health anytime:  roost status");
  } else if (options.shim) {
    console.log(`  Health anytime:  ${options.shim.path} status   (add ~/.bun/bin to PATH for bare \`roost\`)`);
  } else {
    console.log("  Health anytime:  bun apps/roost-cli/src/main.ts status");
  }
}

async function openInitialBrowser(options: {
  endpoint: QuickstartEndpoint;
  databasePath: string;
  browserOnLoopback: boolean;
}): Promise<void> {
  const browserEndpoint = options.browserOnLoopback
    ? { ...options.endpoint, origin: quickstartLoopbackOrigin(options.endpoint) }
    : options.endpoint;
  let browserToken = await mintBrowserToken(options.databasePath, "quickstart-browser");
  logStep("opening the app with a one-shot browser grant");
  try {
    await openQuickstartBrowser(browserEndpoint, browserToken, process.platform);
  } catch {
    console.log(`  Browser opener failed. Open ${browserEndpoint.origin}; rerun \`roost quickstart\` to pair this browser.`);
  } finally {
    browserToken = "";
  }
}

export async function quickstart(args: string[]): Promise<void> {
  const invocation = parseQuickstartOptions(args);
  const freshEndpoint = resolveQuickstartEndpoint(args, process.platform);
  const binaryName = basename(process.execPath).toLocaleLowerCase("en-US");
  const binary = binaryName !== "bun" && binaryName !== "bun.exe";
  const existing = process.platform === "darwin" || process.platform === "linux"
    ? discoverExistingQuickstartInstall(process.platform)
    : null;
  const endpoint = existing
    ? resolveQuickstartEndpoint(args, process.platform, existing.environment)
    : freshEndpoint;

  if (existing) {
    const result = await runExistingQuickstart({
      installed: existing,
      endpoint,
      invocation,
      provisionWorker: async ({ coordinatorUrl, bootstrapToken }) => {
        if (binary) {
          await installWorkerAgent({
            execPath: process.execPath,
            coordUrl: coordinatorUrl,
            bootstrapToken,
            gitSha: ROOST_VERSION,
            cmd: "install",
            coordinatorHost: true,
            log: logStep,
          });
          return;
        }
        await provisionSourceWorker(coordinatorUrl, bootstrapToken);
      },
    });
    if (!result) {
      console.log("\n✓ --dry-run complete (existing service definition rendered; nothing installed).");
      return;
    }
    const promotion = invocation.coordinatorUrl !== null;
    await openInitialBrowser({
      endpoint: result.endpoint,
      databasePath: result.databasePath,
      browserOnLoopback: promotion,
    });
    printCompletion({
      endpoint: result.endpoint,
      remoteAccessVerified: result.remoteAccessVerified,
      binary,
      shim: null,
    });
    return;
  }

  if (process.platform === "win32" && !binary) {
    die("Windows quickstart requires the signed compiled release", "run install-binary.ps1");
  }
  if (invocation.dryRun) {
    if (binary) {
      await installCoordAgent({
        execPath: process.execPath,
        gitSha: ROOST_VERSION,
        cmd: "write-plist",
        env: coordinatorEnvironmentForQuickstart(endpoint),
        log: logStep,
      });
      await installWorkerAgent({
        execPath: process.execPath,
        coordUrl: endpoint.origin,
        gitSha: ROOST_VERSION,
        cmd: "write-plist",
        coordinatorHost: true,
        log: logStep,
      });
    } else {
      await dryRunServiceDefinitions(endpoint);
    }
    console.log("\n✓ --dry-run complete (service definitions generated; nothing installed).");
    return;
  }

  const serviceCredentials = process.platform === "win32"
    ? invocation.windowsServiceCredentialStdin
      ? await readWindowsServiceCredentials()
      : die(
        "Windows service credential frame is required",
        "run quickstart through the signed install-binary.ps1 front door",
      )
    : undefined;
  let windowsInstall: WindowsQuickstartInstall | null = null;
  let windowsPaths: CoordinatorPaths | null = null;
  try {
    windowsPaths = process.platform === "win32" ? coordinatorPaths() : null;
    if (process.platform === "win32" && binary && serviceCredentials && windowsPaths) {
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
        ...endpointEnvironment,
      }
      : undefined;
    if (binary) {
      console.log(`   roost: ${process.execPath} (${ROOST_VERSION})`);
      await installCoordAgent({
        execPath: process.execPath,
        gitSha: ROOST_VERSION,
        credentials: serviceCredentials,
        env: windowsCoordinatorEnvironment ?? endpointEnvironment,
        log: logStep,
      });
    } else {
      console.log(`   bun: ${process.execPath}`);
      if (invocation.force || !nodeFs.existsSync("node_modules")) {
        logStep("bun install");
        if (await runInherit([process.execPath, "install"]) !== 0) die("bun install failed");
      } else {
        logStep("bun install (skipped — node_modules present; --force to reinstall)");
      }
      if (invocation.force || !nodeFs.existsSync(WEB_DIST_INDEX)) {
        logStep("building web SPA (apps/web → dist)");
        if (await runInherit([process.execPath, "x", "vite", "build"], "apps/web") !== 0) {
          die("vite build failed");
        }
      } else {
        logStep("web SPA build (skipped — dist present)");
      }
      logStep("installing coordinator service");
      if (await runInherit(
        ["bash", "apps/coord/scripts/install.sh", "install"],
        undefined,
        endpointEnvironment,
      ) !== 0) die("coord install.sh failed");
    }
    logStep("waiting for coordinator health");
    if (!await waitForCoordHealth(endpoint)) {
      die("coord did not become healthy on its loopback bind", "check logs: roost logs coord");
    }
    if (!await waitForCoordSpa(endpoint)) {
      die("coord became healthy but did not serve the SPA root", "build or restore the installed SPA assets");
    }
    const databasePath = windowsPaths?.database
      ?? discoverExistingQuickstartInstall(process.platform)?.environment.ROOST_COORDINATOR_DB;
    if (!databasePath) throw new Error("installed coordinator service did not declare its database path");
    const workerCoordinatorUrl = endpoint.mode === "local"
      ? quickstartLoopbackOrigin(endpoint)
      : endpoint.origin;
    const workerToken = await mintWorkerToken(databasePath, "quickstart-local-worker");
    if (binary) {
      await installWorkerAgent({
        execPath: process.execPath,
        coordUrl: workerCoordinatorUrl,
        bootstrapToken: workerToken,
        gitSha: ROOST_VERSION,
        coordinatorHost: true,
        coordinatorEnvironment: windowsCoordinatorEnvironment,
        credentials: serviceCredentials,
        log: logStep,
      });
      if (windowsInstall && serviceCredentials) {
        await proveWindowsInstallHealth(windowsInstall, serviceCredentials.account, endpoint.origin);
      }
    } else {
      logStep("deploying local worker");
      await provisionSourceWorker(workerCoordinatorUrl, workerToken);
    }
    logStep("proving local worker registration");
    const workerFingerprint = await waitForWorkerRegistration(databasePath, workerToken);
    if (!workerFingerprint) {
      die(`local worker did not register through ${workerCoordinatorUrl}`, "check logs: roost logs worker");
    }
    if (!await waitForWorkerRoutability(endpoint, workerFingerprint)) {
      die(`local worker ${workerFingerprint.slice(0, 12)} did not become routable`, "check logs: roost logs worker");
    }
    if (windowsInstall) {
      const report = await statusReport({ origin: endpoint.origin });
      printStatusReport(report);
      if (!report.coordAgentLoaded || !report.workerAgentLoaded || !report.coord.reachable) {
        throw new Error("Windows quickstart status proof did not confirm all required services");
      }
    }
    await openInitialBrowser({ endpoint, databasePath, browserOnLoopback: false });
    const shim = binary ? null : installRoostShim(process.cwd());
    printCompletion({
      endpoint,
      remoteAccessVerified: null,
      binary,
      shim,
    });
    if (windowsInstall) await commitWindowsQuickstartInstall(windowsInstall);
  } catch (error) {
    if (windowsInstall) await rollbackWindowsQuickstartInstall(windowsInstall, error);
    throw error;
  } finally {
    await windowsInstall?.lock.release();
    if (serviceCredentials) serviceCredentials.password = undefined;
  }
}

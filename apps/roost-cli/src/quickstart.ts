// `roost quickstart` — the local one-shot installer. It supports either the
// automatic Tailscale convenience topology or an explicit browser-trusted
// HTTPS endpoint. Endpoint and TLS-file validation are deliberately pure/read
// only and run before any install, state, credential, or service mutation.

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
  automaticQuickstartEndpoint,
  coordinatorEnvironmentForQuickstart,
  requireResolvedEndpoint,
  resolveQuickstartEndpoint,
  validateQuickstartTlsFiles,
} from "./quickstart-endpoint.ts";
import {
  die,
  dryRunServiceDefinitions,
  installRoostShim,
  logStep,
  prepareAutomaticQuickstartNetwork,
  openQuickstartBrowser,
  runInherit,
  waitForCoordHealth,
  waitForWorkerRegistration,
} from "./quickstart-runtime.ts";
import {
  beginWindowsQuickstartInstall,
  commitWindowsQuickstartInstall,
  prepareWindowsQuickstartCoordinatorState,
  prepareWindowsQuickstartTls,
  proveWindowsInstallHealth,
  rollbackWindowsQuickstartInstall,
} from "./quickstart-windows-install.ts";
import type { WindowsQuickstartInstall } from "./quickstart-windows-install.ts";
import { coordinatorPaths } from "./quickstart-windows-state.ts";
import type { CoordinatorPaths } from "./quickstart-windows-state.ts";
import {
  ensureTailscale,
  printStatusReport,
  resolveTailscale,
  statusReport,
} from "./status.ts";
import { ROOST_VERSION } from "./version.ts";

const WEB_DIST_INDEX = "apps/web/dist/index.html";

export {
  coordinatorEnvironmentForQuickstart,
  resolveQuickstartEndpoint,
  validateQuickstartTlsFiles,
} from "./quickstart-endpoint.ts";
export type {
  QuickstartEndpoint,
  QuickstartEndpointMode,
  QuickstartTlsFileSystem,
} from "./quickstart-endpoint.ts";
export {
  openQuickstartBrowser,
  waitForCoordHealth,
} from "./quickstart-runtime.ts";
export type {
  QuickstartBrowserLauncher,
  QuickstartHealthDeps,
} from "./quickstart-runtime.ts";

export async function quickstart(args: string[]): Promise<void> {
  // This pair is the no-effect boundary. In particular it precedes Windows
  // credential reads, machine transactions, state preparation, dependency
  // installs, builds, service definitions, and every Tailscale command.
  const selectedEndpoint = resolveQuickstartEndpoint(args, process.env, process.platform);
  validateQuickstartTlsFiles(selectedEndpoint, nodeFs);

  const force = args.includes("--force");
  const dry = args.includes("--dry-run");
  const binaryName = basename(process.execPath).toLocaleLowerCase("en-US");
  const binary = binaryName !== "bun" && binaryName !== "bun.exe";
  if (process.platform === "win32" && !binary) {
    die("Windows quickstart requires the signed compiled release", "run install-binary.ps1");
  }

  let endpoint = selectedEndpoint;
  if (endpoint.mode === "automatic") {
    let fqdn: string;
    if (dry) {
      fqdn = resolveTailscale().fqdn ?? "dry-run.example.ts.net";
      logStep(`--dry-run (service definitions only), tailnet ${fqdn}`);
    } else {
      logStep("checking Tailscale");
      try {
        const ready = await ensureTailscale({
          resolve: resolveTailscale,
          log: (message) => console.log(`   ${message}`),
          sleep: (ms) => Bun.sleep(ms),
          now: Date.now,
          brewInstall: async () => {
            if (Bun.which("brew")) await runInherit(["brew", "install", "tailscale"]);
          },
        });
        fqdn = ready.fqdn;
      } catch (error) {
        die(error instanceof Error ? error.message : String(error));
      }
      console.log(`   tailnet: ${fqdn}`);
    }
    endpoint = automaticQuickstartEndpoint(fqdn, coordinatorPaths().tlsDir);
  } else {
    logStep(`using explicit HTTPS endpoint ${endpoint.origin}`);
  }
  requireResolvedEndpoint(endpoint);
  const coordUrl = endpoint.origin;

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

    let serviceEndpoint = endpoint;
    if (
      endpoint.mode === "explicit"
      && windowsInstall
      && windowsPaths
      && serviceCredentials
    ) {
      const interactiveSid = process.env.ROOST_INTERACTIVE_SID?.trim() ?? "";
      if (!/^S-1-(?:\d+-)+\d+$/.test(interactiveSid)) {
        throw new Error("ROOST_INTERACTIVE_SID is required for protected TLS installation");
      }
      serviceEndpoint = await prepareWindowsQuickstartTls(
        windowsInstall,
        endpoint,
        windowsPaths,
        serviceCredentials.account,
        interactiveSid,
      );
    }

    const endpointEnvironment = coordinatorEnvironmentForQuickstart(
      serviceEndpoint,
      process.platform,
    );
    const windowsCoordinatorEnvironment = windowsPaths
      ? {
        ROOST_COORD_DATA_DIR: windowsPaths.dataDir,
        ROOST_COORD_LOG_DIR: windowsPaths.logDir,
        ROOST_COORDINATOR_DB: windowsPaths.database,
        ROOST_COORDINATOR_AUTHORIZED_KEYS: windowsPaths.authorizedKeys,
        ROOST_COORDINATOR_KEY_PATH: windowsPaths.key,
        ROOST_COORDINATOR_HANDOFF_PATH: windowsPaths.handoff,
        ROOST_COORDINATOR_TLS_DIR: windowsPaths.tlsDir,
        ...endpointEnvironment,
      }
      : undefined;
    const databasePath = windowsPaths?.database ?? coordinatorPaths().database;

    if (binary) {
      console.log(`   roost: ${process.execPath} (${ROOST_VERSION})`);
      if (!dry) {
        await prepareAutomaticQuickstartNetwork(endpoint, force, process.platform);
      }
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
          coordUrl,
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
        die(`coord did not become healthy at ${coordUrl}`, "check logs: roost logs coord");
      }
      console.log(`   coord healthy at ${coordUrl}`);

      workerToken = await mintWorkerToken(databasePath, "quickstart-local-worker");
      await installWorkerAgent({
        execPath: process.execPath,
        coordUrl,
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
          coordUrl,
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

      await prepareAutomaticQuickstartNetwork(endpoint, force, process.platform);
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
        die(`coord did not become healthy at ${coordUrl}`, "check logs: roost logs coord");
      }
      console.log(`   coord healthy at ${coordUrl}`);

      workerToken = await mintWorkerToken(databasePath, "quickstart-local-worker");
      logStep("deploying local worker");
      const priorCoordinatorUrl = process.env.ROOST_COORDINATOR_URL;
      const priorBootstrapToken = process.env.ROOST_BOOTSTRAP_TOKEN;
      const priorAllowDirty = process.env.ROOST_ALLOW_DIRTY;
      try {
        process.env.ROOST_COORDINATOR_URL = coordUrl;
        process.env.ROOST_BOOTSTRAP_TOKEN = workerToken;
        process.env.ROOST_ALLOW_DIRTY = "1";
        await deploy(
          ["localhost", "--allow-unpublished-local"],
          { coordinatorUrl: coordUrl },
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
    logStep("proving local worker registration");
    const workerFingerprint = await waitForWorkerRegistration(databasePath, workerToken);
    if (!workerFingerprint) {
      die("local worker did not register with its one-shot grant", "check logs: roost logs worker");
    }
    workerToken = undefined;
    console.log(`   worker registered (${workerFingerprint.slice(0, 12)})`);

    const report = await statusReport({ mode: endpoint.mode, origin: coordUrl });
    printStatusReport(report);
    if (
      windowsInstall
      && ((report.tailscale.required && !report.tailscale.running)
        || !report.coordAgentLoaded
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
    console.log(`  This machine:    ${coordUrl}`);
    console.log(`  Pair your phone: open ${coordUrl} → Settings → Pair a device → scan the QR`);
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

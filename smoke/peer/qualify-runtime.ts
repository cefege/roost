// Finite Chromium-to-node-datachannel runtime qualification entry point.
// Runtime-only preserves raw adapter proof; normal mode additionally uses the shared packet codec.
// Source mode uses the worker loader; compiled mode stages the exact literal addon before compilation.

import { chromium, type Browser, type Page } from "@playwright/test";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadTerminalPeerNative } from "../../apps/worker/src/terminal/peer/terminal-peer-native.ts";
import type {
  TerminalPeerNativeStageOperation,
  TerminalPeerNativeTarget,
} from "../../scripts/terminal-peer-native-assets.ts";
import type { TerminalPeerNative } from "../../apps/worker/src/terminal/peer/terminal-peer-native.ts";
import {
  createBrowserOfferer,
  type BrowserOfferer,
} from "./browser-offerer.ts";
import { verifyBrowserReceivedMessages } from "./browser-offerer-verification.ts";
import type { ProductionPacketQualification } from "./packet-qualification.ts";
import {
  CONNECTION_TIMEOUT_MS,
  QUALIFICATION_GENERATIONS,
  asQualificationFailure,
  createQualificationMessages,
  qualificationFailure,
  waitForQualification,
  type PeerQualificationError,
  type QualificationGeneration,
} from "./qualification-common.ts";
import { createNativeAnswerer, type NativeAnswerer } from "./native-answerer.ts";

type QualificationMode = "source" | "compile-host" | "compiled-runtime";

interface QualificationOptions {
  mode: QualificationMode;
  runtimeOnly: boolean;
  browser: "chromium";
  stun: "none";
}

interface NativeAssetStager {
  hostTerminalPeerTarget(): TerminalPeerNativeTarget;
  stageTerminalPeerNative<T>(
    target: TerminalPeerNativeTarget,
    operation: TerminalPeerNativeStageOperation<T>,
  ): Promise<T>;
}

const repositoryRoot = join(import.meta.dir, "..", "..");

async function main(): Promise<void> {
  const options = parseQualificationOptions(Bun.argv.slice(2));
  if (options.mode === "compile-host") {
    await runCompiledHostQualification(options);
  } else {
    await runRuntimeQualification(options);
  }
  console.log(`terminal peer runtime qualification passed [mode=${options.mode} generations=${QUALIFICATION_GENERATIONS}]`);
}

function parseQualificationOptions(argumentsList: readonly string[]): QualificationOptions {
  let mode: QualificationMode | undefined;
  let runtimeOnly = false;
  let browser: "chromium" | undefined;
  let stun: "none" | undefined;
  for (const argument of argumentsList) {
    if (argument === "--source") {
      mode = selectQualificationMode(mode, "source");
      continue;
    }
    if (argument === "--compile-host") {
      mode = selectQualificationMode(mode, "compile-host");
      continue;
    }
    if (argument === "--compiled-runtime") {
      mode = selectQualificationMode(mode, "compiled-runtime");
      continue;
    }
    if (argument === "--runtime-only") {
      runtimeOnly = true;
      continue;
    }
    if (argument === "--browser=chromium") {
      browser = "chromium";
      continue;
    }
    if (argument === "--stun=none") {
      stun = "none";
      continue;
    }
    throw qualificationFailure("arguments", "all", "unknown_argument");
  }
  if (!mode) throw qualificationFailure("arguments", "all", "qualification_mode_required");
  if (!browser) throw qualificationFailure("arguments", "all", "chromium_browser_required");
  if (!stun) throw qualificationFailure("arguments", "all", "stun_none_required");
  return { mode, runtimeOnly, browser, stun };
}

function selectQualificationMode(
  current: QualificationMode | undefined,
  requested: QualificationMode,
): QualificationMode {
  if (current && current !== requested) throw qualificationFailure("arguments", "all", "multiple_qualification_modes");
  return requested;
}

async function runRuntimeQualification(options: QualificationOptions): Promise<void> {
  let native: TerminalPeerNative | undefined;
  let browser: Browser | undefined;
  let failure: PeerQualificationError | undefined;
  try {
    native = await loadNativeRuntime();
    browser = await chromium.launch({ headless: true });
    for (let generation = 1; generation <= QUALIFICATION_GENERATIONS; generation++) {
      await qualifyGeneration(browser, native, generation, options.runtimeOnly);
    }
  } catch (error) {
    failure = asQualificationFailure(error, "runtime", "all", "runtime_qualification_failed");
  }
  if (browser) {
    try {
      await browser.close();
    } catch {
      failure ??= qualificationFailure("browser-cleanup", "all", "browser_close_failed");
    }
  }
  if (native) {
    try {
      native.cleanup();
    } catch {
      failure ??= qualificationFailure("native-cleanup", "all", "native_cleanup_failed");
    }
  }
  if (failure) throw failure;
}

async function loadNativeRuntime(): Promise<TerminalPeerNative> {
  try {
    return await loadTerminalPeerNative();
  } catch (error) {
    throw asQualificationFailure(error, "native-load", "all", "native_loader_failed");
  }
}

async function loadProductionPacketQualification(
  generation: QualificationGeneration,
): Promise<ProductionPacketQualification> {
  try {
    // Runtime-only intentionally avoids loading the normal-mode codec path.
    const { createProductionPacketQualification } = await import("./packet-qualification.ts");
    return createProductionPacketQualification(generation);
  } catch (error) {
    throw asQualificationFailure(error, "packet-codec", generation, "packet_codec_load_failed");
  }
}

async function qualifyGeneration(
  browser: Browser,
  native: TerminalPeerNative,
  generation: QualificationGeneration,
  runtimeOnly: boolean,
): Promise<void> {
  let page: Page | undefined;
  let offerer: BrowserOfferer | undefined;
  let answerer: NativeAnswerer | undefined;
  let packetQualification: ProductionPacketQualification | undefined;
  try {
    page = await browser.newPage();
    const messages = createQualificationMessages();
    packetQualification = runtimeOnly ? undefined : await loadProductionPacketQualification(generation);
    const activePacketQualification = packetQualification;
    offerer = await createBrowserOfferer(
      page,
      generation,
      messages,
      activePacketQualification
        ? (lane, packet) => activePacketQualification.receiveNativePacket(lane, packet)
        : undefined,
    );
    answerer = createNativeAnswerer(
      native,
      generation,
      messages,
      offerer.offerFingerprint,
      packetQualification,
    );
    const answerSdp = await answerer.acceptOffer(offerer.offerSdp);
    await offerer.acceptAnswer(answerSdp);
    if (packetQualification) {
      await offerer.waitForChannels();
      await packetQualification.sendBrowserPackets((lane, packet) => offerer!.sendPacket(lane, packet));
      await waitForQualification(
        "packet-messages",
        generation,
        CONNECTION_TIMEOUT_MS,
        packetQualification.waitForTransfers(),
      );
      packetQualification.assertTransfersComplete();
    }
    const transferCompletion = await Promise.all([answerer.waitForMessages(), offerer.waitForMessages()]);
    verifyBrowserReceivedMessages(transferCompletion[1], messages.nativeToBrowser, generation);
    await offerer.close();
    await answerer.close();
  } catch (error) {
    throw asQualificationFailure(error, "generation", generation, "generation_qualification_failed");
  } finally {
    packetQualification?.dispose();
    if (offerer) await offerer.dispose();
    answerer?.dispose();
    if (page) await page.close().catch(() => undefined);
  }
}

async function runCompiledHostQualification(options: QualificationOptions): Promise<void> {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "roost-peer-qualify-"));
  const compiledFixture = join(temporaryRoot, "qualify-runtime");
  const isolatedWorkingDirectory = join(temporaryRoot, "isolated");
  try {
    const nativeAssetStager = await loadNativeAssetStager();
    const target = nativeAssetStager.hostTerminalPeerTarget();
    await nativeAssetStager.stageTerminalPeerNative(target, async () => {
      await compileQualificationFixture(compiledFixture, target);
    });
    // Runtime starts only after staging restores the source stub and removes the
    // addon, so this run proves the executable contains the literal embed.
    await mkdir(isolatedWorkingDirectory);
    await executeCompiledFixture(compiledFixture, isolatedWorkingDirectory, options);
  } catch (error) {
    throw asQualificationFailure(error, "native-stage", "all", "compiled_qualification_failed");
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

async function loadNativeAssetStager(): Promise<NativeAssetStager> {
  try {
    // The emitted fixture must not evaluate staging's source-tree manifest from
    // its isolated cwd; only the source-side compile-host mode needs this module.
    const moduleUrl = new URL("../../scripts/terminal-peer-native-assets.ts", import.meta.url).href;
    return await import(moduleUrl) as NativeAssetStager;
  } catch (error) {
    throw asQualificationFailure(error, "native-stage", "all", "native_asset_stager_unavailable");
  }
}

async function compileQualificationFixture(
  compiledFixture: string,
  target: TerminalPeerNativeTarget,
): Promise<void> {
  await runChildProcess(
    "compiled-build",
    "all",
    [
      process.execPath,
      "build",
      "--compile",
      `--target=${target}`,
      "--define",
      "__ROOST_EMBEDDED_TERMINAL_PEER__=true",
      "--external=chromium-bidi/lib/cjs/bidiMapper/BidiMapper",
      "--external=chromium-bidi/lib/cjs/cdp/CdpConnection",
      join(repositoryRoot, "smoke", "peer", "qualify-runtime.ts"),
      "--outfile",
      compiledFixture,
    ],
    repositoryRoot,
    process.env,
  );
}

async function executeCompiledFixture(
  compiledFixture: string,
  isolatedWorkingDirectory: string,
  options: QualificationOptions,
): Promise<void> {
  const isolatedEnvironment = { ...process.env };
  delete isolatedEnvironment.NODE_PATH;
  await runChildProcess(
    "compiled-runtime",
    "all",
    [
      compiledFixture,
      "--compiled-runtime",
      ...(options.runtimeOnly ? ["--runtime-only"] : []),
      `--browser=${options.browser}`,
      `--stun=${options.stun}`,
    ],
    isolatedWorkingDirectory,
    isolatedEnvironment,
  );
}

async function runChildProcess(
  stage: string,
  generation: QualificationGeneration,
  command: readonly string[],
  cwd: string,
  environment: NodeJS.ProcessEnv,
): Promise<void> {
  let exitCode: number;
  try {
    const child = Bun.spawn({
      cmd: [...command],
      cwd,
      env: environment,
      stdin: "ignore",
      stdout: "inherit",
      stderr: "inherit",
    });
    exitCode = await child.exited;
  } catch (error) {
    throw asQualificationFailure(error, stage, generation, "child_start_failed");
  }
  if (exitCode !== 0) throw qualificationFailure(stage, generation, "child_exit_failed");
}

try {
  await main();
} catch (error) {
  const failure = asQualificationFailure(error, "runtime", "all", "qualification_failed");
  console.error(failure.message);
  process.exitCode = 1;
}

import { mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { startBrowserLeg, type BrowserLeg } from "./browser-leg";
import type { ParityCase } from "./cases";
import type { ScenarioEvidence } from "./coverage";
import { compareCheckpoint, compareNativeContracts } from "./observe";
import { createLauncher, createLegProfile, type LegProfile } from "./profile";
import { startTerminal, type TerminalLeg } from "./pty";
import { startStack, type RoostStack } from "./stack";
import { eventually } from "./runtime";
import { assertProjectionHealthy, captureCaseState, captureCheckpoint, readProvenance, readTrace, serializeError, type SerializedError } from "./runner-evidence";
import { interpolate, interpolateAction, interpolateComparison, interpolateObservation, legTokens } from "./runner-interpolate";
import type { NativeContract, WireReference } from "./coverage-native";

/** `pruneRawTraces` drops a passing case's raw probe traces once result.json holds the compacted provenance. */
export interface CaseRunOptions { repoRoot: string; sourceRoot: string; runRoot: string; index: number; scenario: ParityCase; signal: AbortSignal; keepOpen: boolean; pruneRawTraces: boolean }
export interface CaseResult { id: string; root: string; passed: boolean; evidence: ScenarioEvidence; errors: SerializedError[]; failure?: SerializedError }

function assertActive(signal: AbortSignal): void { signal.throwIfAborted(); }
interface CaptureObservation { channel: string; pointer?: string; occurrence?: number }
async function requestCaptures(profile: LegProfile, observations: readonly CaptureObservation[]): Promise<void> {
  const requests = observations.map(({ channel, pointer, occurrence }) => ({ channel, pointer, occurrence }));
  await writeFile(join(profile.home, ".parity/capture-request.json"), JSON.stringify({ channels: [...new Set(requests.map(request => request.channel))], requests }));
}

async function terminalReady(terminal: TerminalLeg, profile: LegProfile, model: string): Promise<void> {
  await eventually("actual TUI composer first paint", async () => {
    if (terminal.exitCode !== null) throw new Error(`Actual TUI exited ${terminal.exitCode} during startup`);
    const rows = await readTrace(profile.probeTracePath);
    const rendered = rows.some(row => row.type === "visit" && row.method === "render" &&
      (row.receiverClass === "CustomEditor" || typeof row.component === "string" && row.component.endsWith("#CustomEditor") || typeof row.receiverComponent === "string" && row.receiverComponent.endsWith("#CustomEditor")));
    if (!rendered) return undefined;
    const ready = (await readTrace(profile.effectsPath)).some(row => row.channel === "fixture.session-ready");
    const screen = await terminal.snapshot();
    return ready && screen.text.includes(model) && screen.lines.some(line => line.trim()) ? true : undefined;
  }, 60_000);
}

async function keepSurfaceOpen(stack: RoostStack, browser: BrowserLeg, caseRoot: string, signal: AbortSignal): Promise<void> {
  const environmentPath = join(caseRoot, "stack-environment.json");
  await writeFile(environmentPath, `${JSON.stringify(stack.env, null, 2)}\n`, { mode: 0o600, flag: "wx" });
  process.stdout.write(`OMP_PARITY_INTERACTIVE_READY ${JSON.stringify({ origin: stack.origin, conversationURL: browser.page.url(), environmentPath })}\n`);
  if (signal.aborted) return;
  const completion = Promise.withResolvers<void>();
  const shutdown = () => completion.resolve();
  signal.addEventListener("abort", shutdown, { once: true });
  try { await completion.promise; }
  finally { signal.removeEventListener("abort", shutdown); }
}

export async function runCase(options: CaseRunOptions): Promise<CaseResult> {
  const { scenario } = options;
  const caseRoot = join(options.runRoot, "cases", `${String(options.index + 1).padStart(4, "0")}-${scenario.scenario.id}`);
  await mkdir(caseRoot, { recursive: true });
  const evidence: ScenarioEvidence = { id: scenario.scenario.id, covers: [...scenario.scenario.covers], checkpoints: [], visits: [], wire: [] };
  const errors: unknown[] = [];
  const profiles: LegProfile[] = [];
  let terminal: TerminalLeg | undefined;
  let browser: BrowserLeg | undefined;
  let stack: RoostStack | undefined;
  try {
    assertActive(options.signal);
    const workerHome = join(caseRoot, "browser-worker-home");
    const terminalProfile = await createLegProfile({ ...options, workspace: join(caseRoot, "terminal-workspace"), adapter: "terminal" });
    profiles.push(terminalProfile);
    const browserProfile = await createLegProfile({ ...options, workspace: join(workerHome, "fixtures", "workspace"), adapter: "browser" });
    profiles.push(browserProfile);
    const terminalTokens = await legTokens(terminalProfile);
    const browserTokens = await legTokens(browserProfile);
    terminal = await startTerminal(terminalProfile.command, terminalProfile.workspace, terminalProfile.environment,
      join(caseRoot, "terminal.ansi"), 120, 48, { hostTracePath: join(terminalProfile.home, ".parity/host.jsonl") });
    await terminalReady(terminal, terminalProfile, scenario.kind === "builtin" ? scenario.requirements.models.defaultId : "parity-coder");
    assertActive(options.signal);
    const launcher = await createLauncher(options.repoRoot, options.sourceRoot, caseRoot);
    stack = await startStack(options.repoRoot, caseRoot, { launcherPath: launcher, ompEnvironment: browserProfile.environment,
      workerHome, workerCwd: browserProfile.workspace });
    browser = await startBrowserLeg(stack.origin, join(caseRoot, "browser"), join(browserProfile.home, ".parity/host.jsonl"), {
      allowedOrigins: (browserProfile.environment.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").filter(Boolean),
      ...(scenario.kind === "fixture" && scenario.scenario.browserLocale ? { locale: scenario.scenario.browserLocale } : {}),
      ...(browserProfile.environment.OMP_PARITY_LIVE_WAV ? { virtualAudioFile: browserProfile.environment.OMP_PARITY_LIVE_WAV } : {}),
    });
    await stack.pair(browser.page);
    const conversationId = await stack.openConversation(browser.page, browserProfile.workspace);
    await writeFile(join(caseRoot, "conversation.json"), JSON.stringify({ id: conversationId, url: browser.page.url(), origin: stack.origin }));
    const command = scenario.kind === "fixture" ? `/parity-fixture ${scenario.scenario.id}` : scenario.scenario.command;
    assertActive(options.signal);
    const terminalCommand = `\x1b[200~${interpolate(command, terminalTokens)}\x1b[201~\r`;
    terminal.input(terminalCommand);
    const editor = browser.page.getByRole("textbox", { name: "Message OMP", exact: true });
    const browserCommand = interpolate(command, browserTokens);
    await editor.fill(browserCommand);
    await editor.press("Enter");
    evidence.initialStimulus = { terminalInput: terminalCommand, browser: { role: "textbox", name: "Message OMP", value: browserCommand, key: "Enter" } };
    if (scenario.kind === "fixture") {
      await eventually("both actual custom fixture factories mounted", async () => {
        const ready = await Promise.all(profiles.map(async profile => (await readTrace(profile.effectsPath)).some(row => row.channel === "fixture.ready")));
        return ready.every(Boolean) ? true : undefined;
      });
    }
    const terminalLeg = { workspace: terminalProfile.workspace, home: terminalProfile.home, sourceRoot: options.sourceRoot, runRoot: options.runRoot, effectsPath: terminalProfile.effectsPath, terminal };
    const browserLeg = { workspace: browserProfile.workspace, home: browserProfile.home, sourceRoot: options.sourceRoot, runRoot: options.runRoot, effectsPath: browserProfile.effectsPath, browser };
    for (const [index, checkpoint] of scenario.scenario.checkpoints.entries()) {
      assertActive(options.signal);
      const checkpointRoot = join(caseRoot, "checkpoints", `${String(index + 1).padStart(3, "0")}`);
      try {
        const comparisons = checkpoint.comparisons.map(value => interpolateComparison(value, terminalTokens, browserTokens));
        const reference = (value: WireReference): WireReference => ({ ...value,
          ...(value.target ? { target: { selector: interpolate(value.target.selector, browserTokens) } } : {}),
          ...(value.match ? { match: Object.fromEntries(Object.entries(value.match).map(([pointer, item]) => [pointer, typeof item === "string" ? interpolate(item, browserTokens) : item])) } : {}) });
        const nativeContracts: NativeContract[] = (checkpoint.nativeContracts ?? []).map(contract => ({ ...contract,
          request: reference(contract.request), ...(contract.response ? { response: reference(contract.response) } : {}),
          ...(typeof contract.value === "string" ? { value: interpolate(contract.value, browserTokens) } : {}),
          assertions: contract.assertions.map(assertion => {
            const actual = interpolateObservation(assertion.browser, browserTokens);
            if (actual.type === "screen" || actual.type === "ansi") throw new Error("Native conformance cannot manufacture a terminal observation");
            return { ...assertion, browser: actual, ...(typeof assertion.expected === "string" ? { expected: interpolate(assertion.expected, browserTokens) } : {}) };
          }) }));
        const terminalEffects = comparisons.flatMap(value => value.terminal.type === "effect" ? [value.terminal] : []);
        const browserEffects = [...comparisons.flatMap(value => value.browser.type === "effect" ? [value.browser] : []),
          ...nativeContracts.flatMap(contract => contract.assertions.flatMap(assertion => assertion.browser.type === "effect" ? [assertion.browser] : []))];
        await Promise.all([requestCaptures(terminalProfile, terminalEffects), requestCaptures(browserProfile, browserEffects)]);
        if (checkpoint.terminalResize) terminal.resize(checkpoint.terminalResize.columns, checkpoint.terminalResize.rows);
        const browserActions = (checkpoint.browserActions ?? []).map(action => interpolateAction(action, browserTokens));
        for (const action of browserActions) await browser.action(action);
        const terminalInput = checkpoint.terminalInput === undefined ? undefined : interpolate(checkpoint.terminalInput, terminalTokens);
        if (terminalInput !== undefined) terminal.input(terminalInput);
        if (checkpoint.waitForMilliseconds !== undefined) {
          if (!Number.isFinite(checkpoint.waitForMilliseconds) || checkpoint.waitForMilliseconds < 0 || checkpoint.waitForMilliseconds > 30_000) throw new Error("Checkpoint wait must be between 0 and 30000 real milliseconds");
          await Bun.sleep(checkpoint.waitForMilliseconds);
        }
        for (const turn of checkpoint.releaseProviderTurns ?? []) {
          terminalProfile.provider.release(turn);
          browserProfile.provider.release(turn);
        }
        const executed = await compareCheckpoint(comparisons, terminalLeg, browserLeg, evidence.checkpoints);
        const nativeExecuted = await compareNativeContracts(nativeContracts, browserLeg);
        await assertProjectionHealthy(browser);
        await captureCaseState(checkpointRoot, terminal, browser);
        evidence.checkpoints.push(await captureCheckpoint(checkpoint.name, executed, profiles, { terminalInput, browserActions }, nativeExecuted));
        await writeFile(join(checkpointRoot, "comparisons.json"), JSON.stringify(executed, null, 2));
        await writeFile(join(checkpointRoot, "native-contracts.json"), JSON.stringify(nativeExecuted, null, 2));
      } catch (error) {
        evidence.checkpoints.push({ name: checkpoint.name, passed: false, comparisons: [] });
        throw error;
      }
    }
    await assertProjectionHealthy(browser);
    await Promise.all(profiles.map(profile => profile.provider.assertConsumed()));
    if (terminal.exitCode !== null && (scenario.kind !== "builtin" || !scenario.requirements.lifecycle.allowExit || terminal.exitCode !== 0)) {
      throw new Error(`Actual TUI exited unexpectedly with code ${terminal.exitCode}`);
    }
    if (options.keepOpen) await keepSurfaceOpen(stack, browser, caseRoot, options.signal);
  } catch (error) {
    errors.push(error);
    try { await captureCaseState(join(caseRoot, "failure"), terminal, browser); }
    catch (captureError) { errors.push(captureError); }
  } finally {
    // Browser closure cannot terminate hosted ACP; profiles reclaim their own source generations first.
    for (const cleanup of [
      async () => browser?.stop(), async () => terminal?.stop(),
      ...profiles.toReversed().map(profile => async () => { await profile.stop(); await profile.provider.assertConsumed(); }), async () => stack?.stop(),
    ]) { try { await cleanup(); } catch (error) { errors.push(error); } }
    try { Object.assign(evidence, await readProvenance(profiles, evidence.checkpoints)); }
    catch (error) { errors.push(error); }
  }
  if (errors.length && evidence.checkpoints.every(checkpoint => checkpoint.passed)) {
    evidence.checkpoints.push({ name: "runtime and cleanup settlement", passed: false, comparisons: [] });
  }
  const result: CaseResult = { id: evidence.id, root: caseRoot, passed: errors.length === 0, evidence, errors: errors.map(serializeError),
    ...(errors.length ? { failure: serializeError(new AggregateError(errors, `Actual parity case ${evidence.id} failed`)) } : {}) };
  await writeFile(join(caseRoot, "result.json"), JSON.stringify(result, null, 2));
  if (result.passed && options.pruneRawTraces) await Promise.all(profiles.map(profile => rm(profile.probeTracePath, { force: true })));
  return result;
}

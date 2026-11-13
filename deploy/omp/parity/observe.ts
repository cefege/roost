import { isDeepStrictEqual } from "node:util";
import { resolve, sep } from "node:path";
import type { BrowserLeg } from "./browser-leg";
import type { ExecutedCheckpoint, ExecutedComparison } from "./coverage";
import type { TerminalLeg } from "./pty";
import type { Comparison, Observation } from "./scenario";
import { eventually, objectValue } from "./runtime";
import { observedTarget } from "./observation-target";
import { temporalSatisfied } from "./coverage-temporal";
import type { ExecutedNativeContract, NativeContract } from "./coverage-native";
import { pointerValue } from "./observation-pointer";
import { relativeGeometry, terminalRowGap } from "./observation-geometry";

export interface ObservationLeg {
  workspace: string;
  home: string;
  sourceRoot: string;
  runRoot: string;
  effectsPath: string;
  terminal?: TerminalLeg;
  browser?: BrowserLeg;
}


function ownedFile(leg: ObservationLeg, path: string): string {
  const file = resolve(leg.workspace, path);
  if (![leg.workspace, leg.home].some(root => file === resolve(root) || file.startsWith(`${resolve(root)}${sep}`))) throw new Error(`Observation escaped fixture profile: ${path}`);
  return file;
}

async function readObservation(observation: Observation, leg: ObservationLeg, metadata: Pick<ExecutedComparison, "terminalMatch" | "browserTarget">): Promise<unknown> {
  switch (observation.type) {
    case "screen": {
      if (!leg.terminal) throw new Error("Screen observation requires the real PTY leg");
      const snapshot = await leg.terminal.snapshot(observation.read !== undefined && observation.read !== "capture");
      const match = new RegExp(observation.pattern, observation.flags).exec(snapshot.text);
      if (match) metadata.terminalMatch = { text: match[0] };
      if (observation.read === "visible") return match !== null;
      if (!match) return undefined;
      const prefix = snapshot.text.slice(0, match.index);
      const row = prefix.split("\n").length - 1;
      const stringColumn = prefix.length - prefix.lastIndexOf("\n") - 1;
      const column = snapshot.cellColumns?.[row]?.[stringColumn] ?? stringColumn;
      if (["sameRow", "rowGap", "columnGap", "below"].includes(observation.read ?? "")) {
        if (!observation.relativeTo) throw new Error(`${observation.read} requires an actual terminal reference match`);
        const reference = new RegExp(observation.relativeTo.pattern, observation.relativeTo.flags).exec(snapshot.text);
        if (!reference) return undefined;
        const referencePrefix = snapshot.text.slice(0, reference.index);
        const referenceRow = referencePrefix.split("\n").length - 1;
        const referenceStringColumn = referencePrefix.length - referencePrefix.lastIndexOf("\n") - 1;
        const referenceEnd = snapshot.cellColumns?.[referenceRow]?.[referenceStringColumn + reference[0].length];
        const gap = terminalRowGap(row, referenceRow, reference[0].split("\n").length);
        if (observation.read === "sameRow") return row === referenceRow;
        if (observation.read === "below") return gap >= 0;
        if (observation.read === "rowGap") return gap;
        if (reference[0].includes("\n") || referenceEnd === undefined) throw new Error("columnGap requires one actual reference cell line");
        return column - referenceEnd;
      }
      const read = observation.read ?? "capture";
      switch (read) {
        case "capture": return match[observation.group ?? 0];
        case "column": return column;
        case "row": return row;
        case "leftHalf": return column < snapshot.columns / 2;
        case "rightHalf": return column >= snapshot.columns / 2;
        case "topHalf": return row < snapshot.rows / 2;
        case "bottomHalf": return row >= snapshot.rows / 2;
        case "foreground": case "background": return leg.terminal.cellColors(row, column)[read];
        case "bold": case "italic": case "underline": case "inverse": return leg.terminal.cellStyle(row, column)[read];
        case "rightGap": {
          if (match[0].includes("\n")) throw new Error("A terminal rightGap observation must match one visible line");
          const end = snapshot.cellColumns?.[row]?.[stringColumn + match[0].length];
          if (end === undefined) throw new Error("Actual terminal cell boundary is unavailable");
          return snapshot.columns - end;
        }
        case "bottomGap": return snapshot.rows - row - 1;
      }
    }
    case "ansi": {
      if (!leg.terminal) throw new Error("ANSI emission observation requires the real PTY leg");
      const match = new RegExp(observation.pattern, observation.flags).exec(await leg.terminal.readOutput());
      return match?.[observation.group ?? 0];
    }
    case "dom": {
      if (!leg.browser) throw new Error("DOM observation requires the actual HTML leg");
      if (["sameRow", "rowGap", "columnGap", "below"].includes(observation.read)) {
        const value = await relativeGeometry(leg.browser.page, observation.selector, observation.relativeTo, observation.read);
        metadata.browserTarget = await observedTarget(leg.browser.page, observation.selector);
        return value;
      }
      if (observation.read === "naturalWidth" || observation.read === "naturalHeight") {
        const locator = leg.browser.page.locator(observation.selector);
        if (await locator.count() !== 1) return undefined;
        const value = await locator.evaluate((element, read) => element instanceof HTMLImageElement && element.complete
          && element.naturalWidth > 0 && element.naturalHeight > 0 ? element[read as "naturalWidth" | "naturalHeight"] : undefined, observation.read);
        metadata.browserTarget = await observedTarget(leg.browser.page, observation.selector);
        return value;
      }
      const semanticReads = ["bold", "italic", "underline", "inverse", "selected", "pressed", "expanded", "collapsed", "focused", "readonly", "disabled", "mask"];
      if (semanticReads.includes(observation.read)) {
        metadata.browserTarget = await observedTarget(leg.browser.page, observation.selector);
        return metadata.browserTarget?.state[observation.read];
      }
      const value = await leg.browser.observe(observation);
      metadata.browserTarget = await observedTarget(leg.browser.page, observation.selector);
      return value;
    }
    case "effect": {
      if (!await Bun.file(leg.effectsPath).exists()) return undefined;
      const content = await Bun.file(leg.effectsPath).text();
      const complete = content.endsWith("\n") ? content : content.slice(0, content.lastIndexOf("\n") + 1);
      const values = complete.split("\n").filter(Boolean).map(line => objectValue(JSON.parse(line), "fixture effect")).filter(entry => entry.channel === observation.channel).map(entry => entry.value);
      const value = observation.occurrence === undefined ? values.at(-1) : values[observation.occurrence];
      return pointerValue(value, observation.pointer);
    }
    case "file": {
      const file = ownedFile(leg, observation.path);
      if (!await Bun.file(file).exists()) return undefined;
      const value: unknown = observation.pointer === undefined ? await Bun.file(file).text() : await Bun.file(file).json();
      return pointerValue(value, observation.pointer);
    }
  }
}

function normalize(value: unknown, kind: Comparison["normalize"], leg: ObservationLeg): unknown {
  if (kind === undefined) return value;
  if (kind === "number" || kind === "percent-fraction" || kind === "percent-remaining-fraction") {
    const number = typeof value === "number" ? value : typeof value === "string" && value.trim() !== "" ? Number(value) : NaN;
    if (!Number.isFinite(number)) throw new Error(`Observation is not a finite number: ${JSON.stringify(value)}`);
    return kind === "percent-fraction" ? number / 100 : kind === "percent-remaining-fraction" ? 1 - number / 100 : number;
  }
  if (typeof value !== "string") throw new Error(`${kind} normalization requires a string observation`);
  if (kind === "whitespace") return value.replace(/\s+/gu, " ").trim();
  const paths: Array<[string, string]> = [
    [leg.home, "<HOME>"], [leg.workspace, "<WORKSPACE>"],
    [leg.sourceRoot, "<OMP_SOURCE>"], [leg.runRoot, "<RUN>"],
  ];
  let normalized = value;
  for (const [path, token] of paths.sort((first, second) => second[0].length - first[0].length)) normalized = normalized.replaceAll(path, token);
  return normalized;
}

export async function compareCheckpoint(
  comparisons: readonly Comparison[], terminal: ObservationLeg, browser: ObservationLeg, previous: readonly ExecutedCheckpoint[] = [],
): Promise<ExecutedComparison[]> {
  const last = new Map<string, unknown>();
  return eventually("terminal/HTML checkpoint equivalence", async () => {
    await settleTransitions(browser);
    const results: ExecutedComparison[] = [];
    for (const comparison of comparisons) {
      const metadata: Pick<ExecutedComparison, "terminalMatch" | "browserTarget"> = {};
      const rawTerminal = await readObservation(comparison.terminal, terminal, metadata);
      const rawBrowser = await readObservation(comparison.browser, browser, metadata);
      if (rawTerminal === undefined || rawBrowser === undefined) {
        last.set(comparison.name, { terminal: rawTerminal, browser: rawBrowser });
        return undefined;
      }
      const terminalValue = normalize(rawTerminal, comparison.normalize, terminal);
      const browserValue = normalize(rawBrowser, comparison.normalize, browser);
      const hasExpected = Object.hasOwn(comparison, "expected");
      const expected = hasExpected ? normalize(comparison.expected, comparison.normalize?.startsWith("percent-") ? "number" : comparison.normalize, terminal) : undefined;
      const adaptation = comparison.expectedByAdapter && { ...comparison.expectedByAdapter,
        terminal: normalize(comparison.expectedByAdapter.terminal, comparison.normalize?.startsWith("percent-") ? "number" : comparison.normalize, terminal),
        browser: normalize(comparison.expectedByAdapter.browser, comparison.normalize?.startsWith("percent-") ? "number" : comparison.normalize, browser) };
      if (adaptation && hasExpected) throw new Error(`Ambiguous comparison expectations: ${comparison.name}`);
      if (comparison.temporal && (adaptation || hasExpected)) throw new Error(`Temporal comparisons cannot declare fixed or negative expectations: ${comparison.name}`);
      const executed: ExecutedComparison = {
        ...comparison, ...metadata, passed: true, terminalValue, browserValue, terminalRawValue: rawTerminal, browserRawValue: rawBrowser,
        ...(hasExpected ? { expected } : {}), ...(adaptation ? { expectedByAdapter: adaptation } : {}),
      };
      const passed = comparison.temporal ? temporalSatisfied(executed, previous)
        : adaptation
          ? isDeepStrictEqual(terminalValue, adaptation.terminal) && isDeepStrictEqual(browserValue, adaptation.browser)
          : isDeepStrictEqual(terminalValue, browserValue) && (!hasExpected || isDeepStrictEqual(terminalValue, expected));
      last.set(comparison.name, { terminal: terminalValue, browser: browserValue, expected: comparison.temporal ?? adaptation ?? expected });
      if (!passed) return undefined;
      results.push(executed);
    }
    return results;
  }).catch(error => { throw new Error(`Checkpoint observations: ${JSON.stringify(Object.fromEntries(last))}`, { cause: error }); });
}

/**
 * A person reads an opening dialog after its fade, not at opacity 0: wait for every
 * finite CSS animation/transition so visibility and paint reads see the settled frame.
 * Infinite ones (spinners, shimmer) never finish and are left running.
 */
async function settleTransitions(leg: ObservationLeg): Promise<void> {
  await leg.browser?.page.evaluate(() => Promise.all(document.getAnimations()
    .filter(animation => animation.playState === "running" && animation.effect?.getComputedTiming().iterations !== Infinity)
    .map(animation => animation.finished.catch(() => undefined))));
}

export async function compareNativeContracts(contracts: readonly NativeContract[], browser: ObservationLeg): Promise<ExecutedNativeContract[]> {
  return eventually("actual native-only contract consequences", async () => {
    const results: ExecutedNativeContract[] = [];
    for (const contract of contracts) {
      if (!browser.browser) throw new Error("Native conformance requires the actual browser leg");
      const request = { ...contract.request, ...(contract.request.target ? { browserTarget: await observedTarget(browser.browser.page, contract.request.target.selector) } : {}) };
      const response = contract.response && { ...contract.response, ...(contract.response.target ? { browserTarget: await observedTarget(browser.browser.page, contract.response.target.selector) } : {}) };
      if (request.target && !request.browserTarget || response?.target && !response.browserTarget) return undefined;
      const assertions: ExecutedNativeContract["assertions"] = [];
      for (const assertion of contract.assertions) {
        const metadata: Pick<ExecutedComparison, "terminalMatch" | "browserTarget"> = {};
        const raw = await readObservation(assertion.browser, browser, metadata);
        if (raw === undefined) return undefined;
        const actual = normalize(raw, assertion.normalize, browser);
        const expected = normalize(assertion.expected, assertion.normalize?.startsWith("percent-") ? "number" : assertion.normalize, browser);
        if (!isDeepStrictEqual(actual, expected)) return undefined;
        assertions.push({ ...assertion, actual, expected, passed: true, ...(metadata.browserTarget ? { browserTarget: metadata.browserTarget } : {}) });
      }
      results.push({ ...contract, request, ...(response ? { response } : {}), assertions });
    }
    return results;
  });
}

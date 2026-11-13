import type { ExtensionContext, ExtensionCustomOptions, ExtensionUiComponent } from "@omp-source/packages/coding-agent/src/extensibility/extensions/types";
import type { TUI } from "@omp-source/packages/tui/src/tui";
import type { Theme } from "@omp-source/packages/tui/src/theme/theme";
import type { KeybindingsManager } from "@omp-source/packages/tui/src/app-keybindings";
import type { NativeContract } from "./coverage-native";

export interface FixtureContext {
  tui: TUI;
  theme: Theme;
  keybindings: KeybindingsManager;
  extensionContext: ExtensionContext;
  workspace: string;
  load<Module>(relativeSourcePath: string): Promise<Module>;
  record(channel: string, value: unknown): void;
  done(value?: unknown): void;
}

export type BrowserAction =
  | { type: "click"; selector: string }
  | { type: "doubleClick"; selector: string }
  | { type: "fill"; selector: string; value: string }
  | { type: "press"; key: string; selector?: string }
  | { type: "check"; selector: string; checked: boolean }
  | { type: "hover"; selector: string }
  | { type: "select"; selector: string; value: string }
  | { type: "resize"; width: number; height: number }
  | { type: "upload"; selector: string; files: string[] }
  | { type: "transportDelay"; milliseconds: number; direction?: "incoming" | "outgoing" | "both" }
  | { type: "reload" }
  /** Environment change: the browser's media features as `matchMedia` reports them. */
  | { type: "emulateMedia"; reducedMotion?: "reduce" | "no-preference"; colorScheme?: "dark" | "light" }
  /** Native-contract lane: a protocol event addressed to the real node the selector resolves to. */
  | { type: "nativeEvent"; selector: string; event: Record<string, unknown> & { ev: string } }
  /** Native-contract lane: refresh the live transport with the client's own open parameters and these features. */
  | { type: "nativeOpen"; features: string[] };

export type Observation =
  | { type: "screen"; pattern: string; flags?: string; group?: number; relativeTo?: { pattern: string; flags?: string }; read?: "capture" | "column" | "row" | "leftHalf" | "rightHalf" | "topHalf" | "bottomHalf" | "visible" | "rightGap" | "bottomGap" | "foreground" | "background" | "bold" | "italic" | "underline" | "inverse" | "sameRow" | "rowGap" | "columnGap" | "below" }
  | { type: "ansi"; pattern: string; flags?: string; group?: number }
  | {
      type: "dom";
      selector: string;
      read: "text" | "value" | "attribute" | "css" | "count" | "checked" | "selectionStart" | "selectionEnd" | "selectionDirection" | "rectWidth" | "rectHeight" | "rectX" | "rectY" | "scrollTop" | "scrollHeight" | "clientHeight" | "leftHalf" | "rightHalf" | "topHalf" | "bottomHalf" | "insideViewport" | "visible" | "rightGap" | "bottomGap" | "bold" | "italic" | "underline" | "inverse" | "selected" | "pressed" | "expanded" | "collapsed" | "focused" | "readonly" | "disabled" | "mask" | "naturalWidth" | "naturalHeight" | "sameRow" | "rowGap" | "columnGap" | "below";
      attribute?: string;
      property?: string;
      pattern?: string;
      flags?: string;
      group?: number;
      relativeTo?: string;
      unit?: "pixels" | "cells" | "fraction";
    }
  | { type: "effect"; channel: string; occurrence?: number; pointer?: string }
  | { type: "file"; path: string; pointer?: string };

export interface ComparisonReference { checkpoint: string; comparison: string }
export interface ContentPart {
  pointer: string; effect: "literal" | "appearance" | "layout" | "link";
  comparison: ComparisonReference;
}
export interface BehaviorClaim {
  entry: string;
  effect: "literal" | "appearance" | "layout" | "interaction" | "host" | "temporal" | "link" | "content";
  value?: unknown;
  before?: ComparisonReference;
  target?: ComparisonReference;
  assertionGroups?: Array<{ comparisons: ComparisonReference[] }>;
  parts?: ContentPart[];
}

export interface Comparison {
  name: string;
  terminal: Observation;
  browser: Observation;
  expected?: unknown;
  expectedByAdapter?: {
    reason: "native-descriptor-failure" | "unsupported-interactive-component";
    terminal: unknown;
    browser: unknown;
  };
  normalize?: "whitespace" | "number" | "fixture-path" | "percent-fraction" | "percent-remaining-fraction";
  proves?: BehaviorClaim[];
  temporal?: { direction: "sample" } | { from: ComparisonReference; direction: "increase" | "decrease" };
}

export interface Checkpoint {
  name: string;
  terminalInput?: string;
  terminalResize?: { columns: number; rows: number };
  waitForMilliseconds?: number;
  releaseProviderTurns?: number[];
  browserActions?: BrowserAction[];
  comparisons: Comparison[];
  nativeContracts?: NativeContract[];
}

export interface FixtureCase {
  id: string;
  covers: string[];
  behaviors?: string[];
  presentation?: ExtensionCustomOptions;
  /** BCP 47 locale of the browser leg; the client reports the locale's clock format as hello.hour12. */
  browserLocale?: string;
  create(context: FixtureContext): ExtensionUiComponent | Promise<ExtensionUiComponent>;
  checkpoints: Checkpoint[];
}

export interface BuiltinCase {
  id: string;
  covers: string[];
  behaviors?: string[];
  command: string;
  settings?: Record<string, unknown>;
  files?: Record<string, string>;
  checkpoints: Checkpoint[];
}

export function visibleText(
  name: string,
  terminalPattern: string,
  browserSelector: string,
  expected: string,
): Comparison {
  return {
    name,
    terminal: { type: "screen", pattern: terminalPattern, group: 1 },
    browser: { type: "dom", selector: browserSelector, read: "text" },
    expected,
    normalize: "whitespace",
  };
}

export function callbackValue(name: string, channel: string, expected: unknown): Comparison {
  return {
    name,
    terminal: { type: "effect", channel },
    browser: { type: "effect", channel },
    expected,
  };
}

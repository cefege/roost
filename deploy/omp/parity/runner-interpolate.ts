import { join } from "node:path";
import type { LegProfile } from "./profile";
import type { BrowserAction, Comparison, Observation } from "./scenario";
import { objectValue } from "./runtime";

export interface LegTokens { workspace: string; home: string; sessionId?: string; relayPort?: string }

export async function legTokens(profile: LegProfile): Promise<LegTokens> {
  const tokens: LegTokens = { workspace: profile.workspace, home: profile.home };
  const file = Bun.file(join(profile.home, ".parity/builtin-seed.json"));
  if (await file.exists()) {
    const seed = objectValue(await file.json(), "owned builtin manifest");
    if (typeof seed.sessionId === "string") tokens.sessionId = seed.sessionId;
  }
  const relayPort = profile.environment.OMP_PARITY_RELAY_PORT;
  if (relayPort !== undefined) {
    if (!/^\d+$/.test(relayPort) || Number(relayPort) < 1 || Number(relayPort) > 65535) throw new Error("Invalid actual relay port");
    tokens.relayPort = relayPort;
  }
  return tokens;
}

export function interpolate(value: string, tokens: LegTokens): string {
  return value.replace(/\{\{([^{}]+)\}\}/g, (_match, name: string) => {
    if (!["workspace", "home", "sessionId", "relayPort"].includes(name)) throw new Error(`Unknown parity placeholder {{${name}}}`);
    const replacement = tokens[name as keyof LegTokens];
    if (replacement === undefined) throw new Error(`Actual owned runtime has no value for {{${name}}}`);
    return replacement;
  });
}

export function interpolateObservation(value: Observation, tokens: LegTokens): Observation {
  switch (value.type) {
    case "screen": case "ansi": return { ...value, pattern: interpolate(value.pattern, tokens) };
    case "dom": return { ...value, selector: interpolate(value.selector, tokens),
      ...(value.relativeTo === undefined ? {} : { relativeTo: interpolate(value.relativeTo, tokens) }),
      ...(value.pattern === undefined ? {} : { pattern: interpolate(value.pattern, tokens) }) };
    case "effect": return { ...value, channel: interpolate(value.channel, tokens), ...(value.pointer === undefined ? {} : { pointer: interpolate(value.pointer, tokens) }) };
    case "file": return { ...value, path: interpolate(value.path, tokens) };
  }
}

export function interpolateComparison(value: Comparison, terminal: LegTokens, browser: LegTokens): Comparison {
  return { ...value, terminal: interpolateObservation(value.terminal, terminal), browser: interpolateObservation(value.browser, browser),
    ...(typeof value.expected === "string" ? { expected: interpolate(value.expected, terminal) } : {}),
    ...(value.expectedByAdapter ? { expectedByAdapter: { ...value.expectedByAdapter,
      terminal: typeof value.expectedByAdapter.terminal === "string" ? interpolate(value.expectedByAdapter.terminal, terminal) : value.expectedByAdapter.terminal,
      browser: typeof value.expectedByAdapter.browser === "string" ? interpolate(value.expectedByAdapter.browser, browser) : value.expectedByAdapter.browser } } : {}) };
}

export function interpolateAction(value: BrowserAction, tokens: LegTokens): BrowserAction {
  switch (value.type) {
    case "fill": case "select": return { ...value, selector: interpolate(value.selector, tokens), value: interpolate(value.value, tokens) };
    case "click": case "doubleClick": case "check": case "hover": return { ...value, selector: interpolate(value.selector, tokens) };
    case "press": return { ...value, key: interpolate(value.key, tokens), ...(value.selector === undefined ? {} : { selector: interpolate(value.selector, tokens) }) };
    case "upload": return { ...value, selector: interpolate(value.selector, tokens), files: value.files.map(file => interpolate(file, tokens)) };
    case "nativeEvent": return { ...value, selector: interpolate(value.selector, tokens) };
    case "resize": case "transportDelay": case "reload": case "emulateMedia": case "nativeOpen": return value;
  }
}

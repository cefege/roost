// Parses the eight fire-and-forget `roost api ui` commands.
// api-ui.ts owns dispatch; this module removes options before checking arity.
// Argument bytes are preserved while ambiguous or unsupported argv is rejected.

export type LegacyUiSubcommand =
  | "navigate"
  | "place-split"
  | "select-tab"
  | "focus-pane"
  | "move-tab"
  | "arrange"
  | "close-tab"
  | "spotlight";

export interface ParsedLegacyUiArgs {
  subcommand: LegacyUiSubcommand;
  targetTabId: string;
  positionals: string[];
  insertFirst: boolean;
  spotlightOff: boolean;
}

export type LegacyUiCommand = {
  command:
    | { case: "navigate"; value: { path: string } }
    | { case: "placeSplit"; value: {
      sessionId: string;
      anchorSessionId: string;
      dir: string;
      insertFirst: boolean;
    } }
    | { case: "selectTab"; value: { sessionId: string } }
    | { case: "focusPane"; value: { sessionId: string } }
    | { case: "moveTab"; value: { sessionId: string; destSessionId: string } }
    | { case: "arrange"; value: { preset: string } }
    | { case: "closeTab"; value: { sessionId: string } }
    | { case: "spotlight"; value: { sessionId: string; off: boolean } };
};

/** Parse options in any order, then require the command's exact positional arity. */
export function parseLegacyUiArgs(args: readonly string[]): ParsedLegacyUiArgs | null {
  const subcommand = legacyUiSubcommand(args[0]);
  if (subcommand === null) return null;

  const positionals: string[] = [];
  let targetTabId = "";
  let sawTab = false;
  let insertFirst = false;
  let spotlightOff = false;

  for (let index = 1; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--tab") {
      if (sawTab) throw new Error(`ui ${subcommand}: duplicate --tab`);
      const value = args[index + 1];
      if (!value || value.startsWith("--") || value.trim().length === 0) {
        throw new Error(`ui ${subcommand}: --tab requires a nonempty value`);
      }
      targetTabId = value;
      sawTab = true;
      index += 1;
      continue;
    }
    if (argument === "--first" && subcommand === "place-split") {
      if (insertFirst) throw new Error("ui place-split: duplicate --first");
      insertFirst = true;
      continue;
    }
    if (argument === "--off" && subcommand === "spotlight") {
      if (spotlightOff) throw new Error("ui spotlight: duplicate --off");
      spotlightOff = true;
      continue;
    }
    if (argument.startsWith("--")) {
      throw new Error(`ui ${subcommand}: unknown option ${JSON.stringify(argument)}`);
    }
    positionals.push(argument);
  }

  const expectedArity = legacyUiArity(subcommand);
  if (positionals.length !== expectedArity || positionals.some((value) => value.length === 0)) {
    const noun = expectedArity === 1 ? "argument" : "arguments";
    throw new Error(`ui ${subcommand}: expected exactly ${expectedArity} positional ${noun}`);
  }
  return { subcommand, targetTabId, positionals, insertFirst, spotlightOff };
}

function legacyUiSubcommand(value: string | undefined): LegacyUiSubcommand | null {
  switch (value) {
    case "navigate":
    case "place-split":
    case "select-tab":
    case "focus-pane":
    case "move-tab":
    case "arrange":
    case "close-tab":
    case "spotlight":
      return value;
    default:
      return null;
  }
}

function legacyUiArity(subcommand: LegacyUiSubcommand): number {
  switch (subcommand) {
    case "place-split": return 3;
    case "move-tab": return 2;
    default: return 1;
  }
}

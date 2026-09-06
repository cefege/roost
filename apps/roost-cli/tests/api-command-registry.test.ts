// Production API-registry tests parse every executable `roost api` example in
// the canonical skill. The guarded prompt entry must remain the exact metadata
// and parser exported by api-agent-prompt.ts rather than a second flag schema.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  AGENT_PROMPT_API_COMMAND,
  parseAgentPromptArgs,
} from "../src/api-agent-prompt.ts";
import { API_COMMAND_REGISTRY } from "../src/api-command-registry.ts";

const ROOT = resolve(import.meta.dir, "../../..");
const SKILL_PATH = resolve(ROOT, "skills/roost/SKILL.md");

function splitDocumentedShellWords(command: string): string[] {
  const words = command.match(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\S+/g) ?? [];
  return words.map((word) => {
    const quote = word[0];
    if ((quote === "\"" || quote === "'") && word.at(-1) === quote) {
      return word.slice(1, -1).replace(/\\([\\"'])/g, "$1");
    }
    return word;
  });
}

function recognizeDocumentedApiExample(example: string): string {
  const words = splitDocumentedShellWords(example);
  if (words[0] !== "roost" || words[1] !== "api") {
    throw new Error("documented example is not a roost api command");
  }
  const verb = words[2];
  if (!verb) throw new Error("documented API example has no verb");
  const command = API_COMMAND_REGISTRY[verb];
  if (!command) throw new Error(`documented API verb is absent from registry: ${verb}`);
  const args = words.slice(3);
  if ("parseArgs" in command) {
    command.parseArgs(args);
    return command.verb;
  }

  let positionalCount = 0;
  const seenOptions = new Set<string>();
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (!argument.startsWith("--")) {
      positionalCount += 1;
      continue;
    }
    const equalsIndex = argument.indexOf("=");
    const optionName = equalsIndex === -1 ? argument : argument.slice(0, equalsIndex);
    const inlineValue = equalsIndex === -1 ? undefined : argument.slice(equalsIndex + 1);
    const option = command.options[optionName];
    if (!option) throw new Error(`${command.verb}: undocumented option ${optionName}`);
    if (seenOptions.has(optionName)) throw new Error(`${command.verb}: duplicate option ${optionName}`);
    seenOptions.add(optionName);
    if (!option.takesValue) {
      if (inlineValue !== undefined) throw new Error(`${command.verb}: ${optionName} takes no value`);
      continue;
    }
    let optionValue = inlineValue;
    if (optionValue === undefined) {
      index += 1;
      optionValue = args[index];
    }
    if (!optionValue || optionValue.startsWith("--")) {
      throw new Error(`${command.verb}: ${optionName} requires a value`);
    }
  }
  if (
    positionalCount < command.positionalArgs.minimum
    || positionalCount > command.positionalArgs.maximum
  ) {
    throw new Error(`${command.verb}: documented positional arguments do not match metadata`);
  }
  for (const [optionName, option] of Object.entries(command.options)) {
    if (option?.required && !seenOptions.has(optionName)) {
      throw new Error(`${command.verb}: documented example is missing ${optionName}`);
    }
  }
  return command.verb;
}

describe("ROOST skill API command registry", () => {
  test("reuses the guarded-prompt command and parser by identity", () => {
    const registered = API_COMMAND_REGISTRY["agent-prompt"];
    if (!registered || !("parseArgs" in registered)) {
      throw new Error("agent-prompt parser is absent from the API registry");
    }
    expect(registered).toBe(AGENT_PROMPT_API_COMMAND);
    expect(registered.parseArgs).toBe(parseAgentPromptArgs);
  });

  test("recognizes every documented API verb and option pair", () => {
    const examples = readFileSync(SKILL_PATH, "utf8")
      .split("\n")
      .filter((line) => line.startsWith("roost api "));
    expect(examples.length).toBeGreaterThan(0);
    const countByVerb: Record<string, number> = {};

    for (const example of examples) {
      const verb = recognizeDocumentedApiExample(example);
      countByVerb[verb] = (countByVerb[verb] ?? 0) + 1;
    }

    expect(countByVerb).toEqual({
      agents: 2,
      "agent-status": 2,
      "agent-wait": 1,
      "agent-prompt": 2,
    });
  });

  test("omits undocumented verbs and delegates invalid prompt options", () => {
    expect(API_COMMAND_REGISTRY.cells).toBeUndefined();
    const registered = API_COMMAND_REGISTRY["agent-prompt"];
    if (!registered || !("parseArgs" in registered)) {
      throw new Error("agent-prompt parser is absent from the API registry");
    }
    expect(() => registered.parseArgs([
      "$ROOST_SESSION_ID",
      "continue",
      "--wait",
    ])).toThrow("agent-prompt: --wait, --until, and --timeout must be provided together");
  });
});

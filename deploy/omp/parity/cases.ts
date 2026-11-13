import type { BuiltinRequirements } from "./builtin-requirements";
import type { BuiltinCase, FixtureCase } from "./scenario";
import { builtinCases, builtinRequirements } from "./builtin-cases";
import { builtinCases as chatCases, builtinRequirements as chatRequirements } from "./chat-cases";
import { fixtureCases as primitiveCases } from "./fixtures/primitives";
import { fixtureCases as dialogCases } from "./fixtures/dialogs";
import { fixtureCases as pickerCases } from "./fixtures/pickers";
import { fixtureCases as settingCases } from "./fixtures/settings";
import { fixtureCases as setupCases } from "./fixtures/setup";
import { fixtureCases as toolCases } from "./fixtures/tools";
import { fixtureCases as composerCases } from "./fixtures/composers";
import { fixtureCases as segmentCases } from "./fixtures/segments";
import { fixtureCases as overlayCases } from "./fixtures/overlays";
import { fixtureCases as nativeContractCases } from "./fixtures/native-contracts";
import { fixtureCases as componentCases } from "./fixtures/components";

export type ParityCase =
  | { kind: "fixture"; scenario: FixtureCase }
  | { kind: "builtin"; scenario: BuiltinCase; requirements: BuiltinRequirements };

export interface ParityCases {
  all: ParityCase[];
  fixtures: FixtureCase[];
}

export function loadCases(): ParityCases {
  const requirementsById = { ...builtinRequirements, ...chatRequirements };
  for (const id of Object.keys(chatRequirements)) {
    if (Object.hasOwn(builtinRequirements, id)) throw new Error(`Duplicate builtin runtime requirements: ${id}`);
  }
  const fixtures = [
    ...primitiveCases, ...dialogCases, ...pickerCases, ...settingCases, ...setupCases,
    ...toolCases, ...composerCases, ...segmentCases, ...overlayCases, ...nativeContractCases,
    ...componentCases,
  ];
  const all: ParityCase[] = [
    ...fixtures.map(scenario => ({ kind: "fixture" as const, scenario })),
    ...[...builtinCases, ...chatCases].map(scenario => {
      const requirements = requirementsById[scenario.id];
      if (!requirements) throw new Error(`Missing builtin runtime requirements: ${scenario.id}`);
      return { kind: "builtin" as const, scenario, requirements };
    }),
  ];
  const seen = new Set<string>();
  for (const { scenario } of all) {
    if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(scenario.id) || seen.has(scenario.id)) throw new Error(`Invalid or duplicate parity scenario: ${scenario.id}`);
    seen.add(scenario.id);
    // Native-only contracts have no terminal counterpart: such a checkpoint proves its
    // source-bound wire contract instead of a TTY/DOM comparison, and claims no parity covers.
    const nativeOnly = scenario.checkpoints.some(checkpoint => checkpoint.nativeContracts?.length);
    if (!scenario.covers.length && !nativeOnly || !scenario.checkpoints.length
      || scenario.checkpoints.some(checkpoint => !checkpoint.comparisons.length && !checkpoint.nativeContracts?.length)) throw new Error(`Scenario has no complete behavioral proof: ${scenario.id}`);
    const names = scenario.checkpoints.map(checkpoint => checkpoint.name);
    if (new Set(names).size !== names.length) throw new Error(`Duplicate checkpoint: ${scenario.id}`);
  }
  return { all, fixtures };
}

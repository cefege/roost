import type { BuiltinCase } from "./scenario";
import type { BuiltinRequirements } from "./builtin-requirements";
import { defaultBuiltinRequirements } from "./builtin-requirements";
import { builtinCases as contentCases, providerTurns as contentTurns } from "./chat-content-cases";
import { builtinCases as toolCases, providerTurns as toolTurns } from "./chat-tool-cases";
import { builtinCases as controlCases, providerTurns as controlTurns } from "./chat-control-cases";
import { builtinCases as todoLifecycleCases, compactBuiltinCases as todoCompactCases, providerTurns as todoTurns } from "./chat-todo-cases";
import { builtinCases as judgmentCases, providerTurns as judgmentTurns } from "./chat-judgment-cases";
import { builtinCases as attachmentCases, providerTurns as attachmentTurns } from "./chat-attachment-cases";
import { builtinCases as figureCases, providerTurns as figureTurns } from "./chat-figure-cases";
import { builtinCases as componentChatCases, providerTurns as componentChatTurns } from "./chat-component-cases";

const todoCases = [...todoLifecycleCases, ...todoCompactCases];
export const builtinCases: BuiltinCase[] = [...contentCases, ...toolCases, ...controlCases, ...todoCases, ...judgmentCases, ...attachmentCases, ...componentChatCases, ...figureCases];
const turns = { ...contentTurns, ...toolTurns, ...controlTurns, ...todoTurns, ...judgmentTurns, ...attachmentTurns, ...componentChatTurns, ...figureTurns };
export const builtinRequirements: Record<string, BuiltinRequirements> = {};
const identities = new Set<string>();
for (const scenario of builtinCases) {
  if (identities.has(scenario.id)) throw new Error(`Duplicate actual chat scenario ${scenario.id}`);
  identities.add(scenario.id);
  const tape = turns[scenario.id];
  if (!tape?.length) throw new Error(`Missing actual provider tape for ${scenario.id}`);
  const requirement = defaultBuiltinRequirements(scenario);
  requirement.models.turns = tape;
  scenario.settings = { ...scenario.settings, "statusLine.compactThinkingLevel": false };
  if (scenario.id === "chat-unicode-markdown-stream") Object.assign(scenario.settings, { hideThinkingBlock: false, expandThinkingBlocks: true, proseOnlyThinking: true });
  builtinRequirements[scenario.id] = requirement;
}
for (const id of Object.keys(turns)) if (!identities.has(id)) throw new Error(`Unexercised chat provider tape ${id}`);

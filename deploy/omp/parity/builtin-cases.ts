import { BUILTIN_SLASH_COMMAND_DEFS } from "@omp-source/packages/coding-agent/src/slash-commands/builtin-registry";
import type { BuiltinCase } from "./scenario";
import { modeCases } from "./builtin-mode-cases";
import { sessionCases } from "./builtin-session-cases";
import { lifecycleCases } from "./builtin-lifecycle-cases";
import { collaborationCases } from "./builtin-collaboration-cases";
import { packageCases } from "./builtin-package-cases";
import { securityCases } from "./builtin-security-cases";
import { controlCases } from "./builtin-control-cases";
import { mutationCases } from "./builtin-mutation-cases";
import { serviceCases } from "./builtin-service-cases";
import { builtinCompletionCases } from "./builtin-completion-cases";
import { executionCases } from "./builtin-execution-cases";
import { buildBuiltinRequirements } from "./builtin-requirements";
import { annotateBuiltinExecution, deriveBuiltinAliasCases } from "./builtin-obligations";
import { securityWorkflowCases } from "./builtin-security-workflow-cases";
import { applySecurityWorkflowRequirements } from "./builtin-security-workflow-requirements";
import { sessionWorkflowCases } from "./builtin-session-workflow-cases";
import { applySessionWorkflowRequirements } from "./builtin-session-workflow-requirements";
import { planWorkflowCases } from "./builtin-plan-workflow-cases";
import { applyPlanWorkflowRequirements } from "./builtin-plan-workflow-requirements";
import { memoryWorkflowCases } from "./builtin-memory-workflow-cases";
import { applyMemoryWorkflowRequirements } from "./builtin-memory-workflow-requirements";
import { localMemoryWorkflowCases } from "./builtin-local-memory-workflow-cases";
import { applyLocalMemoryWorkflowRequirements } from "./builtin-local-memory-workflow-requirements";
import { agentsWorkflowCases } from "./builtin-agents-workflow-cases";
import { todoWorkflowCases } from "./builtin-todo-workflow-cases";
import { applyAgentsWorkflowRequirements } from "./builtin-agents-workflow-requirements";
import { mcpWorkflowCases } from "./builtin-mcp-workflow-cases";
import { applyMcpWorkflowRequirements } from "./builtin-mcp-workflow-requirements";
import { collabWorkflowCases } from "./builtin-collab-workflow-cases";
import { applyCollabWorkflowRequirements } from "./builtin-collab-workflow-requirements";
import { authWorkflowCases } from "./builtin-auth-workflow-cases";
import { applyAuthWorkflowRequirements } from "./builtin-auth-workflow-requirements";
export type { BuiltinRequirements, BuiltinProviderTurn } from "./builtin-requirements";

const primaryBuiltinCases: BuiltinCase[] = [
  ...modeCases, ...sessionCases, ...lifecycleCases, ...collaborationCases,
  ...packageCases, ...securityCases, ...controlCases, ...mutationCases,
  ...serviceCases, ...executionCases, ...builtinCompletionCases, ...securityWorkflowCases, ...sessionWorkflowCases,
  ...planWorkflowCases, ...memoryWorkflowCases, ...localMemoryWorkflowCases,
  ...agentsWorkflowCases,
  ...todoWorkflowCases,
  ...mcpWorkflowCases,
  ...collabWorkflowCases,
  ...authWorkflowCases,
];
const aliasVariants = deriveBuiltinAliasCases(primaryBuiltinCases);
export const builtinCases: BuiltinCase[] = [
  ...primaryBuiltinCases, ...aliasVariants.map(variant => variant.scenario),
].map(annotateBuiltinExecution);

const ids = new Set<string>();
const executed = new Set<string>();
for (const scenario of builtinCases) {
  if (ids.has(scenario.id)) throw new Error(`Duplicate builtin scenario: ${scenario.id}`);
  ids.add(scenario.id);
  const submissions = [scenario.command];
  for (const checkpoint of scenario.checkpoints) {
    if (!checkpoint.browserActions?.some(action => action.type === "press" && action.key === "Enter")) continue;
    for (const action of checkpoint.browserActions) if (action.type === "fill") submissions.push(action.value);
  }
  for (const submitted of submissions) {
    const token = submitted.match(/^\/([^\s]+)/)?.[1];
    if (token) executed.add(token.startsWith("force:") ? "force:" : token);
  }
}
for (const definition of BUILTIN_SLASH_COMMAND_DEFS) for (const token of [definition.name, ...(definition.aliases ?? [])]) {
  if (!executed.has(token)) throw new Error(`Actual builtin registry has no concrete scenario: /${token}`);
}

export const builtinRequirements = buildBuiltinRequirements(builtinCases);
applySecurityWorkflowRequirements(builtinRequirements);
applySessionWorkflowRequirements(builtinRequirements);
applyPlanWorkflowRequirements(builtinRequirements);
applyMemoryWorkflowRequirements(builtinRequirements);
applyLocalMemoryWorkflowRequirements(builtinRequirements);
applyAgentsWorkflowRequirements(builtinRequirements);
applyMcpWorkflowRequirements(builtinRequirements);
applyCollabWorkflowRequirements(builtinRequirements);
applyAuthWorkflowRequirements(builtinRequirements);
for (const variant of aliasVariants) builtinRequirements[variant.scenario.id] = structuredClone(builtinRequirements[variant.originalId]);

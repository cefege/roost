import type { BuiltinRequirements } from "./builtin-requirements";
import { agentsWorkflowCases } from "./builtin-agents-workflow-cases";

export function applyAgentsWorkflowRequirements(requirements: Record<string, BuiltinRequirements>): void {
  for (const scenario of agentsWorkflowCases) {
    const requirement = requirements[scenario.id];
    if (!requirement) throw new Error(`Missing agents workflow scenario ${scenario.id}`);
    requirement.session.storage = "persisted";
    requirement.models.ids = ["parity-coder", "parity-small"];
    requirement.models.defaultId = "parity-coder";
    requirement.models.reasoning = true;
    requirement.models.roles = {
      ...requirement.models.roles,
      default: "openai/parity-coder", task: "openai/parity-coder", smol: "openai/parity-small", advisor: "openai/parity-small",
    };
    requirement.models.remoteNetwork = "deny";
    requirement.models.turns = [];
    requirement.prerequisites.push(
      "Fresh owned HOME and cwd contain no project/user agent definitions, custom tool plugins, provider credentials or operator configuration. Preserve real bundled discovery: task/agents.ts loadBundledAgents supplies the task agent and the case derives its alphabetic row index from that stock list. Do not replace AgentsHubComponent, createAgentsHubDeps, its ModelBrowser, Settings, ModelRegistry, command handlers or native descriptors.",
      "Dispatch every slash command through the actual editor and builtin-session.ts handlers. Configuration is not an LLM operation: real model registry uses only the owned loopback openai parity-coder/parity-small catalog; zero scripted provider turns, no generated agents, no remote requests.",
    );
    if (scenario.id.startsWith("builtin-agents-workflow-")) {
      requirement.prerequisites.push(
        "Use the actual createAgentsHubDeps persistence path: cfgTaskAgentModelOverrides.setEntry, cfgTaskAgentPrewalk.setEntry, cfgTaskAgentAdvisor.setEntry and cfgTaskDisabledAgents.setMember. Model thinking is the real :high selector suffix, not a fabricated separate agent thinking setting. The task source definition model is @task, with no prewalk/advisor frontmatter; task.prewalk=false makes both cleared auxiliary properties resolve off.",
        "A committed chip/model/pattern saves immediately; Escape does not roll it back. Pattern Escape steps to the property strip, then agent strip, then list. Escape while assigning cancels the whole model browser even when its query is nonempty. Reopening must reload actual saved entries and resolve patterns through createAgentsHubDeps.resolvePatterns/resolveModelOverride, not retained fixture state.",
      );
      for (const capture of requirement.captures) if (capture.channel === "builtin.persisted-settings") {
        capture.source += " Agents workflow compares actual persisted task members at /task/agentModelOverrides/task, /task/agentPrewalk/task, /task/agentAdvisor/task, and ordered disabled names /task/disabledAgents. Await settings.flush and decode registered IDs to their real storage paths. Canonical Settings.deleteByPath prunes the final empty map, so /task/<map>/exists must observe actual own-property absence after clear. Capture only CURRENT checkpoint pointers, never earlier saved scalar pointers after removal; never synthesize empty maps or return scenario settings.";
      }
    }
  }

  const restart = requirements["builtin-agents-workflow-configure-restart"];
  if (!restart) throw new Error("Missing agents restart persistence workflow");
  restart.lifecycle = { allowExit: false, superviseRealRestart: true, resumeSameSession: true };
  restart.prerequisites.push(
    "Supervise the real /restart child exit/relaunch path, retaining the same owned HOME, saved config.yml and persisted session. Do not reseed task override maps on restart. /builtin.process/restartCount is the actual completed process generation, and the reopened /agents preview must resolve parity-small:high plus the committed prewalk/advisor models from disk before clearing them through the real hub.",
  );

  const tools = requirements["builtin-tools-workflow-scroll"];
  if (!tools) throw new Error("Missing tools navigation workflow");
  tools.prerequisites.push(
    "Keep actual stock tools and buildToolsMarkdown(session.agent.state.tools, session.getXdevToolEntries()) intact. tools.xdev=false, memory.backend=off, autolearn.enabled=false, goal.enabled=false, browser.enabled=false disable optional trailing device/memory/generated tool rows through source settings, not custom tool factories. The real stock catalog starts with read and ends with write. ReportPanel must be fullscreen in ANSI when the report exceeds available rows; End/Home scroll its real ScrollView and native omp.report.body. Both checkpoints assert the corresponding real tool label is inside the rendered viewport, not merely in hidden HTML.",
  );

  const context = requirements["builtin-context-workflow-transcript-navigation"];
  if (!context) throw new Error("Missing context content workflow");
  context.session.seed = "transcript";
  context.session.userText = "PARITY_CONTEXT_USER_CONTENT";
  context.session.assistantText = "PARITY_CONTEXT_ASSISTANT_CONTENT";
  context.prerequisites.push(
    "Build the real ContextViewer from computeSessionContextBreakdown + buildSessionContextView. With only the canonical two seeded transcript messages and no custom tools, overview keyboard selection skips disabled empty categories: System Prompt, Built-in Tools, User / Developer Messages, Assistant Messages are the selectable order. Each transcript category has one entry, so Enter opens full actual text directly and Escape returns to overview. Do not inject a ContextViewData fixture or invoke the component's private navigation state.",
  );

  const hotkeys = requirements["builtin-hotkeys-workflow-copy-prompt"];
  if (!hotkeys) throw new Error("Missing documented hotkeys workflow");
  hotkeys.host.clipboard = "owned-memory";
  hotkeys.prerequisites.push(
    "Preserve the stock app.clipboard.copyPrompt=alt+shift+c binding. Show /hotkeys through CommandController: ANSI buildHotkeysMarkdown/ReportPanel and native HotkeysSheetComponent are the actual supported source paths. After dismissal fill the real editor with an unsent draft; Kitty CSI 99;4u and browser Alt+Shift+C must invoke InputController.handleCopyPrompt -> ctx.copyTextInHost. Capture exact actual clipboard bytes and keep session messageCount zero; never replace copy handlers or send the draft to a provider.",
  );
}

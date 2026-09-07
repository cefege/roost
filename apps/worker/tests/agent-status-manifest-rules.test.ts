// Screen-rule contract for the pinned agent manifests: a realistic visible
// grid in, one state + rule id out. Guards the false-state classes the rules
// exist to kill — a codex composer sitting under an old approval echo, the
// codex trust/update gates, cursor's `run … (y)` affordance requirement, and
// copilot's background-agent wait. Drives evaluateManifest (manifest-engine)
// over AGENT_MANIFESTS, the same pair AgentScreenDetector uses per scan.
import { describe, expect, test } from "bun:test";
import {
  evaluateManifest,
  type ManifestDetection,
} from "../src/agent-status/manifest-engine.ts";
import { AGENT_MANIFESTS } from "../src/agent-status/manifests.ts";
import type { BuiltinAgentId } from "../src/agent-status/process-scan.ts";

type ScreenOutcome = Pick<
  ManifestDetection,
  "state" | "matchedRuleId" | "visibleBlocker" | "visibleWorking" | "visibleIdle"
>;

interface ScreenCase {
  readonly name: string;
  readonly agentId: BuiltinAgentId;
  /** Grid rows as readVisibleScreen yields them: trailing cell padding
   *  trimmed, blank rows kept, joined by "\n" with no trailing newline.
   *  Region selectors are line-based, so this shape decides what rules see. */
  readonly rows: readonly string[];
  readonly oscTitle: string;
  readonly expected: ScreenOutcome;
}

const IDLE_BY_TITLE: ScreenOutcome = {
  state: "idle",
  matchedRuleId: "osc_title_idle",
  visibleBlocker: false,
  visibleWorking: false,
  visibleIdle: true,
};

const CODEX_UPDATE_CHOOSER = [
  "Update available! 0.153.0 -> 9.8.7",
  "Run bun add -g @openai/codex to update.",
  "",
  "› 1. Update now",
  "  2. Skip until next version",
  "",
  "Press enter to continue",
];

const SCREEN_CASES: readonly ScreenCase[] = [
  {
    name: "codex idle at its composer ignores an approval echoed earlier in the transcript",
    agentId: "codex",
    oscTitle: "codex — roost",
    rows: [
      "› apply the pending migration",
      "• Ran bash -lc 'ls migrations'",
      "  apply.sql  rollback.sql",
      "• Edited migrations/apply.sql",
      "  Overwrite migrations/apply.sql? [y/n] y",
      "✓ Applied migrations/apply.sql",
      "",
      "› Ask Codex to do anything",
      "",
    ],
    expected: IDLE_BY_TITLE,
  },
  {
    name: "codex weak approval heuristics still fire when the transcript, not the composer, is live",
    agentId: "codex",
    oscTitle: "codex — roost",
    rows: [
      "› apply the pending migration",
      "• Ran bash -lc 'psql -f migrations/apply.sql'",
      "  Apply pending migration to production? [y/n]",
      "",
    ],
    expected: {
      state: "blocked",
      matchedRuleId: "weak_blocker",
      visibleBlocker: false,
      visibleWorking: false,
      visibleIdle: false,
    },
  },
  {
    name: "codex first-run trust-directory prompt is a visible blocker",
    agentId: "codex",
    oscTitle: "codex",
    rows: [
      "> You are in /home/almalinux/repos/roost",
      "",
      "Do you trust the contents of this",
      "directory? Working with untrusted",
      "contents comes with higher risk of",
      "prompt injection. Trusting the",
      "directory allows project-local config,",
      "hooks, and exec policies to load.",
      "",
      "› 1. Yes, continue",
      "  2. No, quit",
      "",
      "Press enter to continue",
    ],
    expected: {
      state: "blocked",
      matchedRuleId: "trust_directory",
      visibleBlocker: true,
      visibleWorking: false,
      visibleIdle: false,
    },
  },
  {
    name: "codex trust-directory text quoted inside the transcript is not a blocker",
    agentId: "codex",
    oscTitle: "codex — roost",
    rows: [
      "› what does the codex trust prompt look like?",
      "• Explored docs/onboarding.md",
      "> You are in /home/almalinux/repos/roost",
      "Do you trust the contents of this directory?",
      "",
    ],
    expected: IDLE_BY_TITLE,
  },
  {
    name: "codex startup update chooser is a visible blocker",
    agentId: "codex",
    oscTitle: "codex",
    rows: [...CODEX_UPDATE_CHOOSER, "", ""],
    expected: {
      state: "blocked",
      matchedRuleId: "startup_update",
      visibleBlocker: true,
      visibleWorking: false,
      visibleIdle: false,
    },
  },
  {
    name: "codex composer below a dismissed update chooser is idle",
    agentId: "codex",
    oscTitle: "codex — roost",
    rows: [...CODEX_UPDATE_CHOOSER, "", "› Ask Codex to do anything", ""],
    expected: IDLE_BY_TITLE,
  },
  {
    name: "cursor plan line beginning with run is not an approval prompt",
    agentId: "cursor",
    oscTitle: "cursor",
    rows: [
      "● I'll run the test suite and report the failures.",
      "",
      "  run the test suite",
      "  ⬡ Thinking",
      "",
      "  ctrl+c to stop",
      "",
    ],
    expected: {
      state: "working",
      matchedRuleId: "stop_hint_working",
      visibleBlocker: false,
      visibleWorking: true,
      visibleIdle: false,
    },
  },
  {
    name: "cursor run approval carrying the (y) affordance is a visible blocker",
    agentId: "cursor",
    oscTitle: "cursor",
    rows: [
      "● Run terminal command",
      "",
      "  → run bun test apps/worker (y)",
      "    run in background (b)",
      "    reject (esc)",
      "",
    ],
    expected: {
      state: "blocked",
      matchedRuleId: "approval_prompt",
      visibleBlocker: true,
      visibleWorking: false,
      visibleIdle: false,
    },
  },
  {
    name: "copilot waiting on background agents is working with no cancel hint on screen",
    agentId: "copilot",
    oscTitle: "copilot",
    rows: [
      "● Delegated 2 tasks to background agents",
      "",
      "◎ Waiting for background agents · 2 running",
      "",
    ],
    expected: {
      state: "working",
      matchedRuleId: "background_agents_working",
      visibleBlocker: false,
      visibleWorking: true,
      visibleIdle: false,
    },
  },
  {
    name: "copilot background-agent wait outranks the generic cancel hint",
    agentId: "copilot",
    oscTitle: "copilot",
    rows: [
      "● Delegated 2 tasks to background agents",
      "",
      "◎ Waiting for background agents",
      "",
      "  esc to cancel · ctrl+c to exit",
      "",
    ],
    expected: {
      state: "working",
      matchedRuleId: "background_agents_working",
      visibleBlocker: false,
      visibleWorking: true,
      visibleIdle: false,
    },
  },
];

describe("pinned manifest screen rules", () => {
  for (const screenCase of SCREEN_CASES) {
    test(screenCase.name, () => {
      const detection = evaluateManifest(AGENT_MANIFESTS[screenCase.agentId], {
        screen: screenCase.rows.join("\n"),
        oscTitle: screenCase.oscTitle,
      });
      expect({
        state: detection.state,
        matchedRuleId: detection.matchedRuleId,
        visibleBlocker: detection.visibleBlocker,
        visibleWorking: detection.visibleWorking,
        visibleIdle: detection.visibleIdle,
      }).toEqual(screenCase.expected);
    });
  }
});

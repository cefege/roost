import type * as Summaries from "@omp-source/packages/tui/src/chat/compaction-summary-message";
import type * as Advisor from "@omp-source/packages/tui/src/chat/advisor-message";
import type * as LateDiagnostics from "@omp-source/packages/tui/src/chat/late-diagnostics-message";
import type * as Skill from "@omp-source/packages/tui/src/chat/skill-message";
import type * as UserMessage from "@omp-source/packages/tui/src/chat/user-message";
import type { FixtureCase } from "../scenario";
import { componentId, shown, shownAs } from "./components-support";

const chat = (file: string, name: string) => componentId(`chat/${file}`, name);
const summaryFile = "compaction-summary-message";
const toggleSelector = (role: string) => `[data-omp-role="${role}"] :is(button, summary, [role="button"], [aria-expanded])`;

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-chat-compaction-summary-expand",
    covers: [chat(summaryFile, "CompactionSummaryMessageComponent"), chat(summaryFile, "SummaryMessageComponent"), chat(summaryFile, "DividerSummary")],
    async create(context) {
      const { CompactionSummaryMessageComponent } = await context.load<typeof Summaries>("packages/tui/src/chat/compaction-summary-message.ts");
      return new CompactionSummaryMessageComponent({ role: "compactionSummary", summary: "Fixture compaction summary body", tokensBefore: 256000, tokensAfter: 20000, method: "soft", timestamp: 1 } as never);
    },
    checkpoints: [
      { name: "collapsed compaction divider", comparisons: [shownAs("compaction label", "s(oft-compacted)", "S(oft-compacted)", "oft-compacted")] },
      { name: "expand compaction summary", browserActions: [{ type: "click", selector: toggleSelector("omp.compaction") }], comparisons: [shownAs("compaction label after expand", "s(oft-compacted)", "S(oft-compacted)", "oft-compacted")] },
    ],
  },
  {
    id: "comp-chat-branch-summary-expand",
    covers: [chat(summaryFile, "BranchSummaryMessageComponent"), chat(summaryFile, "SummaryMessageComponent")],
    async create(context) {
      const { BranchSummaryMessageComponent } = await context.load<typeof Summaries>("packages/tui/src/chat/compaction-summary-message.ts");
      return new BranchSummaryMessageComponent({ role: "branchSummary", summary: "Fixture branch summary body", fromId: "entry-1", timestamp: 1 });
    },
    checkpoints: [
      { name: "collapsed branch divider", comparisons: [shownAs("branch label", "b(ranch)\\s+·", "B(ranch) summarized", "ranch")] },
      { name: "expand branch summary", browserActions: [{ type: "click", selector: toggleSelector("omp.branch") }], comparisons: [shownAs("branch label after expand", "b(ranch)\\s+·", "B(ranch) summarized", "ranch")] },
    ],
  },
  {
    id: "comp-chat-handoff-summary-expand",
    covers: [chat(summaryFile, "HandoffSummaryMessageComponent"), chat(summaryFile, "SummaryMessageComponent")],
    async create(context) {
      const { HandoffSummaryMessageComponent } = await context.load<typeof Summaries>("packages/tui/src/chat/compaction-summary-message.ts");
      return new HandoffSummaryMessageComponent({ role: "custom", customType: "handoff", content: "<handoff-context>Fixture handoff document</handoff-context>", display: true, timestamp: 1 });
    },
    checkpoints: [
      { name: "collapsed handoff divider", comparisons: [shownAs("handoff label", "h(anded)-off\\s+·", "H(anded) off", "anded")] },
      { name: "expand handoff summary", browserActions: [{ type: "click", selector: toggleSelector("omp.handoff") }], comparisons: [shownAs("handoff label after expand", "h(anded)-off\\s+·", "H(anded) off", "anded")] },
    ],
  },
  {
    id: "comp-chat-late-diagnostics-expand",
    covers: [chat("late-diagnostics-message", "LateDiagnosticsMessageComponent")],
    async create(context) {
      const { LateDiagnosticsMessageComponent } = await context.load<typeof LateDiagnostics>("packages/tui/src/chat/late-diagnostics-message.ts");
      return new LateDiagnosticsMessageComponent([{ path: "src/fixture.ts", summary: "1 error", errored: true, messages: ["fixture.ts:3:1 error TS2322 fixture diagnostic message"] }]);
    },
    checkpoints: [
      { name: "collapsed late diagnostics", comparisons: [shown("diagnostic message", "fixture diagnostic message")] },
      { name: "expand late diagnostics", browserActions: [{ type: "click", selector: toggleSelector("omp.diagnostics.late") }], comparisons: [shown("diagnostic message after expand", "fixture diagnostic message")] },
    ],
  },
  {
    id: "comp-chat-skill-message-expand",
    covers: [chat("skill-message", "SkillMessageComponent")],
    async create(context) {
      const { SkillMessageComponent } = await context.load<typeof Skill>("packages/tui/src/chat/skill-message.ts");
      return new SkillMessageComponent({ role: "custom", customType: "skill-prompt", content: "Fixture skill prompt body", display: true, details: { name: "fixture-skill", path: "/tmp/fixture-skill/SKILL.md", args: "with arguments", lineCount: 12 }, timestamp: 1 } as never);
    },
    checkpoints: [
      { name: "collapsed skill card", comparisons: [shown("skill name", "fixture-skill")] },
      { name: "expand skill prompt", browserActions: [{ type: "click", selector: toggleSelector("omp.skill.prompt") }], comparisons: [shown("skill name after expand", "fixture-skill")] },
    ],
  },
  {
    id: "comp-chat-collapsed-synthetic-expand",
    covers: [chat("user-message", "CollapsedSyntheticMessageComponent"), chat("user-message", "SyntheticSummary")],
    async create(context) {
      const { CollapsedSyntheticMessageComponent } = await context.load<typeof UserMessage>("packages/tui/src/chat/user-message.ts");
      return new CollapsedSyntheticMessageComponent("# Fixture synthetic heading\n\nSynthetic body line one\nSynthetic body line two");
    },
    checkpoints: [
      { name: "collapsed synthetic input", comparisons: [shown("synthetic heading", "Fixture synthetic heading")] },
      { name: "expand synthetic input", browserActions: [{ type: "click", selector: toggleSelector("omp.user.synthetic") }], comparisons: [shown("synthetic heading after expand", "Fixture synthetic heading")] },
    ],
  },
  {
    id: "comp-chat-advisor-message-expand",
    covers: [chat("advisor-message", "AdvisorHeader"), chat("advisor-message", "AdvisorNotes"), chat("advisor-message", "createAdvisorMessageCard")],
    async create(context) {
      const { createAdvisorMessageCard } = await context.load<typeof Advisor>("packages/tui/src/chat/advisor-message.ts");
      const notes = [
        { note: "Fixture advisor blocker note", severity: "blocker" as const },
        { note: "Fixture advisor concern note", severity: "concern" as const },
        { note: "Fixture advisor nit note", severity: "nit" as const },
        { note: "Fixture advisor trailing note" },
      ];
      let expanded = false;
      const card = createAdvisorMessageCard({ notes }, () => expanded, context.theme);
      return Object.assign(card, { handleInput: () => { expanded = true; card.invalidate?.(); context.tui.requestRender(); } });
    },
    checkpoints: [
      { name: "collapsed advisor card", comparisons: [shown("advisor label", "Advisor"), shown("advisor blocker tally", "blocker"), shown("first advisor note", "Fixture advisor blocker note")] },
      { name: "expand advisor card", terminalInput: "x", browserActions: [{ type: "click", selector: toggleSelector("omp.advisor") }], comparisons: [shown("first advisor note after expand", "Fixture advisor blocker note"), shown("trailing advisor note after expand", "Fixture advisor trailing note")] },
    ],
  },
  {
    id: "comp-chat-advisor-single-note",
    covers: [chat("advisor-message", "AdvisorHeader"), chat("advisor-message", "AdvisorNotes"), chat("advisor-message", "createAdvisorMessageCard")],
    async create(context) {
      const { createAdvisorMessageCard } = await context.load<typeof Advisor>("packages/tui/src/chat/advisor-message.ts");
      return createAdvisorMessageCard({ notes: [{ note: "Fixture lone advisor note", severity: "concern" as const }] }, () => false, context.theme);
    },
    checkpoints: [
      { name: "single advisor note", comparisons: [shown("advisor label", "Advisor"), shown("lone advisor note", "Fixture lone advisor note")] },
    ],
  },
];

import type * as JobsPanel from "@omp-source/packages/tui/src/overlays/jobs-panel";
import type { FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-elapsed-stopped-job",
    covers: ["kind:elapsed", "property:TspElapsedProps.stopped", "component:packages/tui/src/overlays/jobs-panel.ts#JobsSheet"],
    async create(context) {
      const { JobsSheet } = await context.load<typeof JobsPanel>("packages/tui/src/overlays/jobs-panel.ts");
      const job: JobsPanel.JobsPanelJob = { id: "settled-job", type: "bash", label: "printf settled", intent: "Inspect settled output", status: "completed", startTime: 1_794_000_000_000, endTime: 1_794_000_012_000 };
      return new JobsSheet({
        load: () => ({ running: [], recent: [job] }),
        inspect: () => ({ command: "printf settled", cwd: context.workspace, pids: [], output: "Settled output" }),
        cancel: () => {},
        close: () => context.done(),
      });
    },
    checkpoints: [{ name: "settled job keeps its run time", comparisons: [{
      name: "run time shares the job's row",
      terminal: { type: "screen", pattern: "12.0s", relativeTo: { pattern: "Inspect settled output" }, read: "sameRow" },
      browser: { type: "dom", selector: '[data-omp-role="omp.jobs.age"]', relativeTo: '[data-omp-role="omp.jobs.id"]', read: "sameRow" },
      expected: true,
      proves: [{ entry: "property:TspElapsedProps.stopped", effect: "layout", value: 12_000 }],
    }] }],
  },
];

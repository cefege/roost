import type * as Dashboard from "@omp-source/packages/tui/src/overlays/usage-dashboard";
import type * as Reports from "@omp-source/packages/coding-agent/src/modes/controllers/command-controller";
import type { UsageReport } from "@omp-source/packages/ai/src/usage";
import type { FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";
import { layoutClaims, todayHeatmap } from "./primitive-data-support";

const windowRow = '[data-omp-role="omp.usage.window"]';
const grid = todayHeatmap();

const limitsTable = '[data-omp-role="omp.usage.limits"]';
const heatmap = '[data-omp-node$="heatmap"]';
const quotaReport: UsageReport = {
  provider: "anthropic",
  fetchedAt: 1794000000000,
  metadata: { email: "fixture@example.invalid" },
  limits: [{
    id: "fixture-week", label: "Fixture quota",
    scope: { provider: "anthropic", accountId: "fixture-account", windowId: "week" },
    window: { id: "week", label: "Weekly" }, amount: { usedFraction: 0.5, unit: "percent" }, status: "ok",
  }],
};

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-usage-chart-table-meter",
    covers: [
      "kind:chart", "kind:table", "kind:meter",
      "property:TspChartProps.kind", "property:TspChartProps.size", "property:TspChartProps.token", "property:TspChartProps.rows", "property:TspChartProps.cells",
      "property:TspChartProps.cols", "property:TspChartProps.cols[].at", "property:TspChartProps.cols[].label", "property:TspChartProps.summary",
      "property:TspTableColumn.align", "property:TspTableColumn.truncate", "property:TspTableColumn.grow", "property:TspTableColumn.id", "property:TspTableColumn.priority",
      "property:TspMeterProps.style", "property:TspMeterProps.size",
      "component:packages/tui/src/overlays/usage-dashboard.ts#UsageDashboardComponent",
    ],
    async create(context) {
      const { UsageDashboardComponent } = await context.load<typeof Dashboard>("packages/tui/src/overlays/usage-dashboard.ts");
      const { renderUsageReports } = await context.load<typeof Reports>("packages/coding-agent/src/modes/controllers/command-controller.ts");
      const date = new Date();
      const day = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
      return new UsageDashboardComponent({
        reports: [quotaReport],
        renderDetail: (width, reports) => renderUsageReports(reports, context.theme, Date.now(), width),
        loadActivity: async push => { push([{ day, cost: 12, requests: 7 }]); },
        refresh: async () => [quotaReport],
        requestRender: () => context.tui.requestRender(),
        onClose: () => context.done(),
      });
    },
    checkpoints: [
      { name: "overview meter and activity heatmap", comparisons: [
        {
          name: "quota meter shares the quota row",
          terminal: { type: "screen", pattern: "50%", relativeTo: { pattern: "Fixture quota" }, read: "sameRow" },
          browser: { type: "dom", selector: `${windowRow} [role="meter"]`, relativeTo: `${windowRow} [data-omp-role="omp.usage.label"]`, read: "sameRow" },
          expected: true,
          proves: [
            { entry: "property:TspMeterProps.style", effect: "layout", value: "bar" },
            { entry: "property:TspMeterProps.size", effect: "layout", value: "md" },
          ],
        },
        contentComparison("heatmap summary", "Activity (\\$12 · 7 requests · last 53 weeks)", `${heatmap} section > p`, "$12 · 7 requests · last 53 weeks", [{ entry: "property:TspChartProps.summary", effect: "literal", value: "$12 · 7 requests · last 53 weeks" }]),
        contentComparison("first heatmap month", `(${grid.cols[0]!.label})`, `${heatmap} thead th:nth-child(2)`, grid.cols[0]!.label, [{ entry: "property:TspChartProps.cols[].label", effect: "literal", value: grid.cols[0]!.label }]),
        {
          name: "weekday rows render beneath the month labels",
          terminal: { type: "screen", pattern: "M ·", relativeTo: { pattern: grid.cols[0]!.label }, read: "below" },
          browser: { type: "dom", selector: `${heatmap} tbody tr:nth-child(1) th`, relativeTo: `${heatmap} thead th:nth-child(2)`, read: "below" },
          expected: true,
          proves: layoutClaims("TspChartProps", { kind: "heatmap", size: "md", token: "accent", rows: ["M", "T", "W", "T", "F", "S", "S"], cells: grid.cells, cols: grid.cols, "cols[].at": grid.cols[0]!.at }),
        },
      ] },
      { name: "details quota table", terminalInput: "d", browserActions: [{ type: "click", selector: '[role="tab"]:has-text("Details")' }], comparisons: [
        {
          name: "limit label and left share share a table row",
          terminal: { type: "screen", pattern: "Fixture quota", relativeTo: { pattern: "50%" }, read: "sameRow" },
          browser: { type: "dom", selector: `${limitsTable} td:nth-child(1)`, relativeTo: `${limitsTable} td:nth-child(3)`, read: "sameRow" },
          expected: true,
          proves: [
            { entry: "property:TspTableColumn.align", effect: "layout", value: "end" },
            { entry: "property:TspTableColumn.truncate", effect: "layout", value: "middle" },
            { entry: "property:TspTableColumn.grow", effect: "layout", value: 1 },
            { entry: "property:TspTableColumn.id", effect: "layout", value: "limit" },
            { entry: "property:TspTableColumn.priority", effect: "layout", value: 4 },
          ],
        },
      ] },
    ],
  },
];

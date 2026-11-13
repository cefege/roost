import { writeFileSync } from "node:fs";
import type * as Reset from "@omp-source/packages/tui/src/overlays/reset-usage-selector";
import type * as Dashboard from "@omp-source/packages/tui/src/overlays/usage-dashboard";
import type * as TurnRow from "@omp-source/packages/tui/src/overlays/usage-row";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Reports from "@omp-source/packages/coding-agent/src/modes/controllers/command-controller";
import type { UsageReport } from "@omp-source/packages/ai/src/usage";
import type { Usage } from "@omp-source/packages/ai/src/types";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

const quota = (fraction: number): UsageReport => ({ provider: "anthropic", fetchedAt: 1794000000000, metadata: { email: "fixture@example.invalid" }, limits: [{ id: "fixture-week", label: "Fixture quota", scope: { provider: "anthropic", accountId: "fixture-account", windowId: "week" }, window: { id: "week", label: "Weekly" }, amount: { usedFraction: fraction, unit: "percent" }, status: "ok" }] });

export const fixtureCases: FixtureCase[] = [
  ...(["commit", "cancel", "unavailable"] as const).map((mode): FixtureCase => ({
    id: `picker-reset-usage-${mode}`, covers: ["component:packages/tui/src/overlays/reset-usage-selector.ts#ResetUsageSelectorComponent"],
    async create(context) {
      const { ResetUsageSelectorComponent } = await context.load<typeof Reset>("packages/tui/src/overlays/reset-usage-selector.ts");
      return new ResetUsageSelectorComponent([{ label: "Fixture account", provider: "anthropic", providerLabel: "Anthropic", availableCount: 2, redeemableCount: mode === "unavailable" ? 0 : 1, target: { provider: "anthropic", credentialId: 11, creditId: "fixture-credit" }, active: true, unavailableReason: mode === "unavailable" ? "Fixture reset cannot be spent" : undefined }], account => { const receipt = { credentialId: account.target.credentialId, creditId: account.target.creditId }; writeFileSync(`${context.workspace}/reset-request.json`, JSON.stringify(receipt)); context.record("reset", receipt); context.done(); }, () => { context.record("cancel", true); context.done(); });
    },
    checkpoints: [
      { name: "account information", comparisons: [visibleText("account", "(Fixture account)", '[data-omp-picker-item] :text-is("Fixture account")', "Fixture account")] },
      { name: "activation requires confirmation or explains refusal", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Spend reset")' }], comparisons: mode === "unavailable" ? [{ name: "refusal", terminal: { type: "screen", pattern: "(Fixture reset cannot be spent)", group: 1 }, browser: { type: "dom", selector: '[data-omp-node="picker"]', read: "text", pattern: "(Fixture reset cannot be spent)", group: 1 }, expected: "Fixture reset cannot be spent" }] : [{ name: "spend target", terminal: { type: "screen", pattern: "spend[\\s\\S]*?(Fixture account)", group: 1 }, browser: { type: "dom", selector: "[data-omp-picker-confirm]", read: "text", pattern: "(Fixture account)", group: 1 }, expected: "Fixture account" }] },
      ...(mode === "cancel" ? [{ name: "cancel confirmation retains account", terminalInput: "\x1b", browserActions: [{ type: "press" as const, key: "Escape" }], comparisons: [visibleText("retained account", "(Fixture account)", '[data-omp-picker-item] :text-is("Fixture account")', "Fixture account")] }] : []),
      { name: mode === "commit" ? "commit reset selection" : "close owner", terminalInput: mode === "commit" ? "\r" : "\x1b", browserActions: mode === "commit" ? [{ type: "click", selector: "[data-omp-picker-confirm] button:text-is(\"Spend\")" }] : [{ type: "press", key: "Escape" }], comparisons: mode === "commit" ? [callbackValue("request identity", "reset", { credentialId: 11, creditId: "fixture-credit" }), { name: "persisted request", terminal: { type: "file", path: "reset-request.json", pointer: "/creditId" }, browser: { type: "file", path: "reset-request.json", pointer: "/creditId" }, expected: "fixture-credit" }] : [callbackValue("cancel", "cancel", true)] },
    ],
  })),
  ...(["refresh", "refresh-error", "activity-error"] as const).map((mode): FixtureCase => ({
    id: `dialog-usage-dashboard-${mode}`, covers: ["component:packages/tui/src/overlays/usage-dashboard.ts#UsageDashboardComponent", ...(mode === "activity-error" ? [] : ["kind:chart"]), "kind:meter", "property:TspMeterProps.value"],
    behaviors: ["usage:used-fraction-versus-remaining-percent", `usage:${mode}`],
    async create(context) {
      const { UsageDashboardComponent } = await context.load<typeof Dashboard>("packages/tui/src/overlays/usage-dashboard.ts");
      const { renderUsageReports } = await context.load<typeof Reports>("packages/coding-agent/src/modes/controllers/command-controller.ts");
      const date = new Date(); const day = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
      return new UsageDashboardComponent({ reports: [quota(0.5)], renderDetail: (width, reports) => renderUsageReports(reports, context.theme, Date.now(), width), loadActivity: async push => { if (mode === "activity-error") throw new Error("Fixture history unavailable"); push([{ day, cost: 12, requests: 7 }]); }, refresh: async () => { context.record("refresh", true); if (mode === "refresh-error") throw new Error("Fixture refresh refused"); return [quota(0.25)]; }, requestRender: () => context.tui.requestRender(), onClose: () => { context.record("closed", true); context.done(); } });
    },
    checkpoints: [
      { name: "actual quota and activity", comparisons: [
        { name: "remaining quota fraction", terminal: { type: "screen", pattern: "(50)%", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.usage.provider"] [data-omp-role="omp.usage.pct"]', read: "text", pattern: "(50)%", group: 1 }, expected: 0.5, normalize: "percent-remaining-fraction", proves: [{ entry: "property:TspMeterProps.value", effect: "literal", value: 0.5, target: { checkpoint: "actual quota and activity", comparison: "actual quota meter owner" } }] },
        { name: "actual quota meter owner", terminal: { type: "screen", pattern: "50%", read: "visible" }, browser: { type: "dom", selector: '[data-omp-role="omp.usage.provider"] [role="meter"]', read: "visible" }, expected: true },
        ...(mode === "activity-error" ? [{ name: "activity error", terminal: { type: "screen" as const, pattern: "(Fixture history unavailable)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.usage.activity"]', read: "text" as const, pattern: "(Fixture history unavailable)", group: 1 }, expected: "Fixture history unavailable" }] : [{ name: "real heatmap activity", terminal: { type: "screen" as const, pattern: "(7 requests)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-node$="heatmap"]', read: "text" as const, pattern: "(7 requests)", group: 1 }, expected: "7 requests" }]),
      ] },
      { name: "refresh", terminalInput: "r", browserActions: [{ type: "click", selector: 'button:text-is("Refresh")' }], comparisons: [
        callbackValue("refresh callback", "refresh", true),
        mode === "refresh-error"
          ? { name: "refresh failure", terminal: { type: "screen", pattern: "(Fixture refresh refused)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.usage.head"]', read: "text", pattern: "(Fixture refresh refused)", group: 1 }, expected: "Fixture refresh refused" }
          : { name: "refreshed remaining quota fraction", terminal: { type: "screen", pattern: "(75)%", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.usage.provider"] [data-omp-role="omp.usage.pct"]', read: "text", pattern: "(75)%", group: 1 }, expected: 0.25, normalize: "percent-remaining-fraction", proves: [{ entry: "property:TspMeterProps.value", effect: "literal", value: 0.25, target: { checkpoint: "refresh", comparison: "actual refreshed meter owner" } }, { entry: "property:TspMeterProps.value", effect: "host", value: 0.25, before: { checkpoint: "actual quota and activity", comparison: "remaining quota fraction" }, target: { checkpoint: "refresh", comparison: "actual refreshed meter owner" }, assertionGroups: [{ comparisons: [{ checkpoint: "refresh", comparison: "refreshed remaining quota fraction" }, { checkpoint: "refresh", comparison: "refresh callback" }] }] }] },
        ...(mode === "refresh-error" ? [] : [{ name: "actual refreshed meter owner", terminal: { type: "screen" as const, pattern: "75%", read: "visible" as const }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.usage.provider"] [role="meter"]', read: "visible" as const }, expected: true }]),
      ] },
      { name: "close", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  })),
  {
    id: "dialog-turn-usage-rate", covers: ["component:packages/tui/src/overlays/usage-row.ts#UsageRowBlock", "kind:rate", "property:TspRateProps.value"],
    behaviors: ["usage:actual-throughput"],
    async create(context) {
      const { createUsageRowBlock } = await context.load<typeof TurnRow>("packages/tui/src/overlays/usage-row.ts");
      const { Container, Input } = await context.load<typeof Ui>("packages/tui/src/index.ts");
      const usage: Usage = { input: 120, output: 80, cacheRead: 20, cacheWrite: 0, totalTokens: 220, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
      const root = new Container(); const input = new Input(); root.addChild(createUsageRowBlock(usage, 2000, 500)); root.addChild(input); input.onSubmit = () => { context.record("closed", true); context.done(); }; return Object.assign(root, { handleInput: (data: string) => input.handleInput(data) });
    },
    checkpoints: [
      { name: "real usage totals and throughput", comparisons: [{ name: "input tokens", terminal: { type: "screen", pattern: "(120)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.usage.turn"]', read: "text", pattern: "in (120)", group: 1 }, expected: 120, normalize: "number" }, { name: "throughput", terminal: { type: "screen", pattern: "(40\\.0)/s", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.usage.turn"] span:text-is("40.0 tok/s")', read: "text", pattern: "(40\\.0)\\s*tok/s", group: 1 }, expected: 40, normalize: "number", proves: [{ entry: "property:TspRateProps.value", effect: "literal", value: 40 }] }] },
      { name: "close owner", terminalInput: "\r", browserActions: [{ type: "press", key: "Enter", selector: "input" }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
];

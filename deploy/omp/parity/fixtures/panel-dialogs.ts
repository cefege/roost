import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Loader from "@omp-source/packages/tui/src/overlays/bordered-loader";
import type * as ErrorBanner from "@omp-source/packages/tui/src/overlays/error-banner";
import type * as Stats from "@omp-source/packages/tui/src/overlays/stats-notice";
import type * as Fireworks from "@omp-source/packages/tui/src/overlays/codex-reset-fireworks";
import type * as Side from "@omp-source/packages/tui/src/overlays/btw-panel";
import type * as Rule from "@omp-source/packages/tui/src/overlays/omfg-panel";
import type * as Hook from "@omp-source/packages/tui/src/overlays/hook-selector";
import type { Component } from "@omp-source/packages/tui/src/tui";
import { callbackValue, visibleText, type FixtureCase, type FixtureContext } from "../scenario";

export async function panelControls(context: FixtureContext, panel: Component, actions: Record<string, () => void>) {
  const { Container } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { HookSelectorComponent } = await context.load<typeof Hook>("packages/tui/src/overlays/hook-selector.ts");
  const root = new Container();
  const finish = () => { panel.dispose?.(); context.record("closed", true); context.done(); };
  const selector = new HookSelectorComponent("Fixture owner actions", [...Object.keys(actions), "Close fixture"], choice => {
    if (choice === "Close fixture") finish(); else actions[choice]?.();
  }, finish);
  root.addChild(panel); root.addChild(selector);
  return Object.assign(root, { handleInput: (data: string) => selector.handleInput(data) });
}

export const fixtureCases: FixtureCase[] = [
  {
    id: "dialog-bordered-loader-abort", covers: ["component:packages/tui/src/overlays/bordered-loader.ts#BorderedLoader"],
    async create(context) {
      const { BorderedLoader } = await context.load<typeof Loader>("packages/tui/src/overlays/bordered-loader.ts");
      const loader = new BorderedLoader(context.tui, context.theme, "Loading fixture information");
      loader.onAbort = () => { context.record("aborted", loader.signal.aborted); loader.dispose(); context.done(); }; return loader;
    },
    checkpoints: [
      { name: "real loading message", comparisons: [visibleText("loading", "(Loading fixture information)", '[data-omp-role="omp.hook.loader"] :text-is("Loading fixture information")', "Loading fixture information")] },
      { name: "abort", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("abort signal", "aborted", true)] },
    ],
  },
  {
    id: "dialog-error-banner-expand-clear", covers: ["component:packages/tui/src/overlays/error-banner.ts#ErrorBannerComponent"],
    async create(context) {
      const { Container, Input } = await context.load<typeof Ui>("packages/tui/src/index.ts");
      const { ErrorBannerComponent } = await context.load<typeof ErrorBanner>("packages/tui/src/overlays/error-banner.ts");
      const root = new Container(); const input = new Input();
      const clear = () => { root.removeChild(banner); context.record("cleared", true); context.done(); };
      const banner = new ErrorBannerComponent("Fixture request failed\nFixture retry detail", clear);
      input.onSubmit = clear; root.addChild(banner); root.addChild(input);
      return Object.assign(root, { handleInput: (data: string) => input.handleInput(data) });
    },
    checkpoints: [
      { name: "error headline", comparisons: [visibleText("headline", "(Fixture request failed)", '[data-omp-role="omp.errorBanner"] :text-is("Fixture request failed")', "Fixture request failed")] },
      { name: "expanded details", browserActions: [{ type: "click", selector: 'button:text-is("Details")' }], comparisons: [{ name: "detail", terminal: { type: "screen", pattern: "(Fixture retry detail)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.errorBanner"]', read: "text", pattern: "(Fixture retry detail)", group: 1 }, expected: "Fixture retry detail" }] },
      { name: "owner clears error", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:text-is("Dismiss")' }], comparisons: [callbackValue("removed banner", "cleared", true)] },
    ],
  },
  {
    id: "dialog-stats-notice-read-close", covers: ["component:packages/tui/src/overlays/stats-notice.ts#StatsNotice"],
    async create(context) {
      const { StatsNotice } = await context.load<typeof Stats>("packages/tui/src/overlays/stats-notice.ts");
      return panelControls(context, new StatsNotice("Fixture statistics are ready"), {});
    },
    checkpoints: [
      { name: "real status", comparisons: [visibleText("notice", "(Fixture statistics are ready)", '[data-omp-role="omp.stats"]', "Fixture statistics are ready")] },
      { name: "close owner", terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="Close fixture"' }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
  {
    id: "dialog-codex-fireworks-close", covers: ["component:packages/tui/src/overlays/codex-reset-fireworks.ts#CodexResetFireworksComponent"],
    async create(context) {
      const { Container } = await context.load<typeof Ui>("packages/tui/src/index.ts");
      const { CodexResetFireworksController } = await context.load<typeof Fireworks>("packages/tui/src/overlays/codex-reset-fireworks.ts");
      const owner = new CodexResetFireworksController({ ui: {
        terminal: context.tui.terminal, requestRender: () => context.tui.requestRender(), setFocus: component => context.tui.setFocus(component),
        showOverlay: (component, options) => { const overlay = context.tui.showOverlay(component, options); const hide = overlay.hide.bind(overlay); return Object.assign(overlay, { hide: () => { hide(); context.record("closed", true); context.done(); } }); },
      } });
      setTimeout(() => { context.record("opened", owner.show({ kind: "saved-reset-banked", added: 1, available: 2 })); context.record("duplicate", owner.show({ kind: "unscheduled-weekly-reset" })); }, 0);
      return Object.assign(new Container(), { dispose: () => owner.dispose() });
    },
    checkpoints: [
      { name: "real celebration and exclusive ownership", comparisons: [{ name: "saved reset information", terminal: { type: "screen", pattern: "(New reset banked · 2 available)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.overlay.fireworks"]', read: "text", pattern: "(New reset banked · 2 available)", group: 1 }, expected: "New reset banked · 2 available", normalize: "whitespace" }, callbackValue("opened", "opened", true), callbackValue("second owner refused", "duplicate", false)] },
      { name: "close nested overlay", terminalInput: "\x1b", browserActions: [{ type: "click", selector: 'button:text-is("Close")' }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  },
  ...(["answer", "error"] as const).map((mode): FixtureCase => ({
    id: `dialog-btw-panel-${mode}`, covers: ["component:packages/tui/src/overlays/btw-panel.ts#BtwPanelComponent"],
    async create(context) {
      const { BtwPanelComponent } = await context.load<typeof Side>("packages/tui/src/overlays/btw-panel.ts");
      const panel = new BtwPanelComponent({ question: "Explain fixture replay", tui: context.tui, canBranch: () => false });
      panel.appendText("Fixture answer prefix");
      return panelControls(context, panel, { "Settle fixture answer": () => { if (mode === "answer") { panel.appendText(" and retained suffix"); panel.markComplete(); context.record("answer", panel.getCopyText()); } else { panel.markError("Fixture side answer refused"); context.record("error", true); } } });
    },
    checkpoints: [
      { name: "streamed prefix", comparisons: [visibleText("prefix", "(Fixture answer prefix)", 'text="Fixture answer prefix"', "Fixture answer prefix")] },
      { name: "actual settlement mutation", terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="Settle fixture answer"' }], comparisons: mode === "answer" ? [visibleText("retained answer", "(Fixture answer prefix and retained suffix)", 'text="Fixture answer prefix and retained suffix"', "Fixture answer prefix and retained suffix"), callbackValue("copyable answer", "answer", "Fixture answer prefix and retained suffix")] : [visibleText("failure", "(Fixture side answer refused)", 'text="Fixture side answer refused"', "Fixture side answer refused"), callbackValue("error settled", "error", true)] },
      { name: "close owner", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  })),
  ...(["saved", "error", "cancelled"] as const).map((mode): FixtureCase => ({
    id: `dialog-omfg-panel-${mode}`, covers: ["component:packages/tui/src/overlays/omfg-panel.ts#OmfgPanelComponent"],
    async create(context) {
      const { OmfgPanelComponent } = await context.load<typeof Rule>("packages/tui/src/overlays/omfg-panel.ts");
      const panel = new OmfgPanelComponent({ complaint: "Keep fixture acknowledgements", tui: context.tui }); panel.appendDraft("Fixture rule candidate");
      return panelControls(context, panel, { "Settle fixture rule": () => { if (mode === "saved") panel.markSaved("fixture-rule.md"); else if (mode === "error") panel.markError("Fixture rule rejected"); else panel.markAborted(); context.record("settled", mode); } });
    },
    checkpoints: [
      { name: "draft information", comparisons: [visibleText("candidate", "(Fixture rule candidate)", 'text="Fixture rule candidate"', "Fixture rule candidate")] },
      { name: "settlement", terminalInput: "\r", browserActions: [{ type: "click", selector: 'text="Settle fixture rule"' }], comparisons: [visibleText("settlement information", mode === "saved" ? "(Saved fixture-rule\\.md)" : mode === "error" ? "(Fixture rule rejected)" : "(Cancelled\\.)", mode === "saved" ? 'text="Saved fixture-rule.md"' : mode === "error" ? 'text="Fixture rule rejected"' : 'text="Cancelled."', mode === "saved" ? "Saved fixture-rule.md" : mode === "error" ? "Fixture rule rejected" : "Cancelled."), callbackValue("state", "settled", mode)] },
      { name: "close owner", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("closed", "closed", true)] },
    ],
  })),
];

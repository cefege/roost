import type * as SessionAccounts from "@omp-source/packages/tui/src/overlays/session-account-selector";
import type * as LogoutAccounts from "@omp-source/packages/tui/src/overlays/logout-account-selector";
import type * as Plugins from "@omp-source/packages/tui/src/overlays/plugin-selector";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

export const fixtureCases: FixtureCase[] = [
  ...(["select", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-session-account-${mode}`,
    covers: ["component:packages/tui/src/overlays/session-account-selector.ts#SessionAccountSelectorComponent"],
    async create(context) {
      const { SessionAccountSelectorComponent } = await context.load<typeof SessionAccounts>("packages/tui/src/overlays/session-account-selector.ts");
      return new SessionAccountSelectorComponent("Fixture provider", [
        { position: 1, credentialId: 11, label: "Fixture primary", active: true }, { position: 2, credentialId: 12, label: "Fixture secondary", active: false },
      ], account => { context.record("account", account.credentialId); context.done(); }, () => { context.record("cancel", true); context.done(); });
    },
    checkpoints: [
      { name: "account information", comparisons: [visibleText("account", "(Fixture secondary)", '[data-omp-picker-item="12"] :text-is("Fixture secondary")', "Fixture secondary")] },
      { name: mode, terminalInput: mode === "select" ? "\x1b[B\r" : "\x1b", browserActions: mode === "select" ? [{ type: "click", selector: '[data-omp-picker-item="12"]' }, { type: "click", selector: 'button:has-text("Pin")' }] : [{ type: "press", key: "Escape" }], comparisons: [callbackValue("outcome", mode === "select" ? "account" : "cancel", mode === "select" ? 12 : true)] },
    ],
  })),
  ...(["select", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-logout-account-${mode}`,
    covers: ["component:packages/tui/src/overlays/logout-account-selector.ts#LogoutAccountSelectorComponent"],
    async create(context) {
      const { LogoutAccountSelectorComponent } = await context.load<typeof LogoutAccounts>("packages/tui/src/overlays/logout-account-selector.ts");
      return new LogoutAccountSelectorComponent("Fixture provider", [
        { credentialId: 11, provider: "fixture-provider", label: "Fixture primary", detail: "Fixture organization", type: "oauth", active: true },
        { credentialId: 12, provider: "fixture-provider", label: "Fixture secondary", detail: "Fixture project", type: "api_key", active: false },
      ], account => { context.record("logout-account", account.credentialId); context.done(); }, () => { context.record("cancel", true); context.done(); });
    },
    checkpoints: [
      { name: "logout target information", comparisons: [visibleText("account", "(Fixture secondary)", '[data-omp-picker-item="12"] :text-is("Fixture secondary")', "Fixture secondary")] },
      { name: mode, terminalInput: mode === "select" ? "\x1b[B\r" : "\x1b", browserActions: mode === "select" ? [{ type: "click", selector: '[data-omp-picker-item="12"]' }, { type: "press", key: "Enter" }] : [{ type: "press", key: "Escape" }], comparisons: [callbackValue("outcome", mode === "select" ? "logout-account" : "cancel", mode === "select" ? 12 : true)] },
    ],
  })),
  ...(["select", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-plugin-${mode}`,
    covers: ["component:packages/tui/src/overlays/plugin-selector.ts#PluginSelectorComponent"],
    async create(context) {
      const { PluginSelectorComponent } = await context.load<typeof Plugins>("packages/tui/src/overlays/plugin-selector.ts");
      const selector = new PluginSelectorComponent(1, [{ plugin: { name: "fixture-plugin", version: "1.0.0", description: "Inspect fixture data" }, marketplace: "fixture-market", scope: "project" }], new Set(), {
        onSelect: (plugin, marketplace, scope) => { context.record("selection", { plugin, marketplace, scope }); context.done(); }, onCancel: () => { context.record("cancel", true); context.done(); },
      });
      return { render: width => selector.render(width), invalidate: () => selector.invalidate(), describe: cx => selector.describe(cx), handleInput: data => selector.getSelectList().handleInput(data), handleNativeEvent: event => selector.handleNativeEvent(event) };
    },
    checkpoints: [
      { name: "marketplace entry", comparisons: [visibleText("plugin", "(fixture-plugin)", '[data-omp-picker-item] :text-is("fixture-plugin")', "fixture-plugin")] },
      { name: mode, terminalInput: mode === "select" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: `button:has-text("${mode === "select" ? "Install" : "Close"}")` }], comparisons: [callbackValue("outcome", mode === "select" ? "selection" : "cancel", mode === "select" ? { plugin: "fixture-plugin", marketplace: "fixture-market", scope: "project" } : true)] },
    ],
  })),
];

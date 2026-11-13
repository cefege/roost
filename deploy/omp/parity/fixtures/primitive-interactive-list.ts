import type * as Dashboard from "@omp-source/packages/tui/src/overlays/usage-dashboard";
import type * as Reports from "@omp-source/packages/coding-agent/src/modes/controllers/command-controller";
import type { UsageReport } from "@omp-source/packages/ai/src/usage";
import type * as GitApp from "@omp-source/packages/tui/src/apps/git/git-tui";
import type { ChangedFile, GitTuiModel } from "@omp-source/packages/tui/src/apps/git/state";
import type * as SelectModule from "@omp-source/packages/tui/src/components/select-list";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/tui-adapters";
import type { FixtureCase } from "../scenario";
import { contentComparison } from "./gallery-support";

const surface = '[id^="omp-view-"][data-omp-surface]';
const alphaRow = '[data-omp-node="l.list/alpha"] button';
const tabs = '[data-omp-node$="tabs"]';
const selectedTab = `${tabs} [role="tab"][aria-selected="true"]`;
const quota: UsageReport = { provider: "anthropic", fetchedAt: 1794000000000, metadata: { email: "fixture@example.invalid" }, limits: [{ id: "fixture-week", label: "Fixture quota", scope: { provider: "anthropic", accountId: "fixture-account", windowId: "week" }, window: { id: "week", label: "Weekly" }, amount: { usedFraction: 0.5, unit: "percent" }, status: "ok" }] };

/** The real usage dashboard's Overview/Details switch: the TUI names the active view in its panel title, the page in a segmented tab. */
const usageTabs: FixtureCase = {
  id: "primitive-tabs-usage-details",
  covers: ["kind:tabs", "property:TspTabsProps.items", "property:TspTabsProps.items[].label", "property:TspTabsProps.active", "component:packages/tui/src/overlays/usage-dashboard.ts#UsageDashboardComponent"],
  async create(context) {
    const { UsageDashboardComponent } = await context.load<typeof Dashboard>("packages/tui/src/overlays/usage-dashboard.ts");
    const { renderUsageReports } = await context.load<typeof Reports>("packages/coding-agent/src/modes/controllers/command-controller.ts");
    return new UsageDashboardComponent({ reports: [quota], renderDetail: (width, reports) => renderUsageReports(reports, context.theme, Date.now(), width), loadActivity: async push => { push([]); }, requestRender: () => context.tui.requestRender(), onClose: () => { context.record("closed", true); context.done(); } });
  },
  checkpoints: [
    { name: "switch to details", terminalInput: "\r", browserActions: [{ type: "click", selector: `${tabs} [role="tab"][data-omp-item="detail"]` }], comparisons: [
      contentComparison("active details label", "(Details)", selectedTab, "Details", [{ entry: "property:TspTabsProps.items[].label", effect: "literal", value: "Details", target: { checkpoint: "switch to details", comparison: "active details label" } }]),
      { name: "active tab shares the title row", terminal: { type: "screen", pattern: "Usage · Details", read: "topHalf" }, browser: { type: "dom", selector: selectedTab, read: "topHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspTabsProps.active", effect: "layout", value: "detail" }, { entry: "property:TspTabsProps.items", effect: "layout", value: [{ id: "overview", label: "Overview" }, { id: "detail", label: "Details" }] }] },
    ] },
  ],
};

const gitFile = '[data-omp-role="omp.app.git.file-d0"]';

/** The real git sidebar row: its status letter is the item's `hint` and its change count the item's `value`, both inside one file row. */
const gitRowHintValue: FixtureCase = {
  id: "primitive-item-hint-value-git-file",
  covers: ["property:TspItemProps.hint", "property:TspItemProps.value", "component:packages/tui/src/apps/git/sidebar.ts#Sidebar"],
  async create(context) {
    const { showGitOverlay } = await context.load<typeof GitApp>("packages/tui/src/apps/git/git-tui.ts");
    const files: ChangedFile[] = [{ path: "fixture.bin", kind: "modified", area: "unstaged", additions: 1, deletions: 0 }];
    const model: GitTuiModel = {
      cwd: context.workspace, branch: "fixture-branch", clean: false, unstaged: files, staged: [], headCommit: null,
      refresh: async () => false, loadChangeStats: async () => false, loadHeadFiles: async () => false,
      streamContents: async () => ({ kind: "asset", old: { kind: "binary", byteLength: 12 }, new: { kind: "binary", byteLength: 14 } }),
      stage: async () => undefined, unstage: async () => undefined, discard: async () => undefined, commit: async () => undefined, applyPatch: async () => undefined,
    };
    let opened = false;
    const open = () => {
      if (opened) return;
      opened = true;
      queueMicrotask(() => { void showGitOverlay(context.tui, { model, createAvatarSource: () => ({ get: () => null }), aiStage: async () => { throw new Error("Fixture forbids AI staging"); }, generateCommitMessage: async () => { throw new Error("Fixture forbids model generation"); } }).then(() => context.done()); });
    };
    return { render: () => { open(); return []; }, invalidate: () => undefined, describe: () => { open(); return null; } };
  },
  checkpoints: [{ name: "changed file row", comparisons: [
    contentComparison("file label", "M (fixture\\.bin)", `${gitFile} button > span:nth-child(1)`, "fixture.bin"),
    contentComparison("change count", "fixture\\.bin\\s+(\\+1)", `${gitFile} button > span.tabular-nums`, "+1", [{ entry: "property:TspItemProps.value", effect: "literal", value: "+1", target: { checkpoint: "changed file row", comparison: "change count" } }]),
    contentComparison("status letter", "(M)(?= fixture\\.bin)", `${gitFile} kbd`, "M", [{ entry: "property:TspItemProps.hint", effect: "content", value: ["M"], target: { checkpoint: "changed file row", comparison: "status letter" }, parts: [{ pointer: "/0", effect: "literal", comparison: { checkpoint: "changed file row", comparison: "status letter" } }] }]),
  ] }],
};

/** A real select list whose rows carry a named icon and a live state: the terminal leads with the glyph and embeds the state in the description. */
const iconStateRows: FixtureCase = {
  id: "primitive-item-icon-state-rows",
  covers: ["property:TspItemProps.icon", "property:TspItemProps.value", "component:packages/tui/src/components/select-list.ts#SelectList"],
  async create(context) {
    const { SelectList } = await context.load<typeof SelectModule>("packages/tui/src/components/select-list.ts");
    const { getSelectListTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
    return new SelectList([
      { value: "alpha", label: "Alpha choice", icon: "◆", iconName: "check", description: "First available · ready", nativeDetail: "First available", state: "ready" },
      { value: "beta", label: "Beta choice", icon: "◇", iconName: "warn", description: "Second available · paused", nativeDetail: "Second available", state: "paused" },
    ], 5, getSelectListTheme());
  },
  checkpoints: [{ name: "icon and state rows", comparisons: [
    contentComparison("alpha state", "First available · (ready)", `${alphaRow} > span.tabular-nums`, "ready", [{ entry: "property:TspItemProps.value", effect: "literal", value: "ready", target: { checkpoint: "icon and state rows", comparison: "alpha state" } }]),
    { name: "alpha icon leads the row", terminal: { type: "screen", pattern: "◆", read: "leftHalf" }, browser: { type: "dom", selector: `${alphaRow} svg`, read: "leftHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspItemProps.icon", effect: "layout", value: "check", target: { checkpoint: "icon and state rows", comparison: "alpha icon leads the row" } }] },
  ] }],
};

export const fixtureCases: FixtureCase[] = [usageTabs, gitRowHintValue, iconStateRows];

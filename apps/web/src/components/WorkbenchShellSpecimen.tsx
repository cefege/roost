// WorkbenchShellSpecimen renders the static workbench reference used by `/design`.
// It demonstrates the same shell regions, tab states, status surface, and menu primitives.
// DesignGallery owns the surrounding token catalog; this component owns only the specimen.
// It depends on the shared M3 primitives and canonical workbench theme tokens.

import type { Component } from "solid-js";
import {
  Button,
  IconButton,
  Icon,
  StatusDot,
  Surface,
} from "./Settings/md/primitives";

export const WorkbenchShellSpecimen: Component = () => (
  <Surface
    level={0}
    radius="xs"
    border
    style={{
      display: "grid",
      "grid-template-columns": "var(--workbench-activity-width) minmax(var(--md-space-9), 1fr) minmax(var(--md-space-9), 2fr)",
      "grid-template-rows": "var(--workbench-titlebar-height) minmax(calc(var(--md-space-9) * 4), 1fr) var(--workbench-statusbar-height)",
      "grid-template-areas": "\"titlebar titlebar titlebar\" \"activity sidebar editor\" \"statusbar statusbar statusbar\"",
      "min-height": "calc(var(--md-space-9) * 6)",
      overflow: "hidden",
      background: "var(--workbench-editor)",
    }}
  >
    <header class="workbench-titlebar" style={{ "grid-column": "1 / -1", display: "flex", "align-items": "center", gap: "var(--md-space-3)", padding: "0 var(--md-space-3)", background: "var(--workbench-titlebar)" }}>
      <Icon name="terminal" filled />
      <span style={{ "font-size": "var(--md-title-s-size)", "font-weight": "var(--md-title-s-weight)" }}>Roost</span>
      <span style={{ color: "var(--text-lo)", "font-size": "var(--md-label-m-size)" }}>Sessions</span>
      <span style={{ margin: "0 auto", color: "var(--text-mid)", "font-size": "var(--md-label-m-size)" }}>Search commands</span>
      <IconButton icon="help" label="Help" />
    </header>
    <nav class="workbench-activity-bar" aria-label="Workbench activity" style={{ display: "flex", "flex-direction": "column", "align-items": "center", gap: "var(--md-space-2)", padding: "var(--md-space-2)", background: "var(--workbench-activity)" }}>
      <Icon style={{ color: "var(--workbench-active)" }} name="terminal" filled />
      <Icon name="search" />
      <Icon name="folder" />
      <span style={{ flex: 1 }} />
      <Icon name="settings" />
    </nav>
    <aside class="workbench-sidebar-region workbench-sidebar" style={{ padding: "var(--md-space-3)", background: "var(--workbench-sidebar)" }}>
      <div style={{ display: "flex", "justify-content": "space-between", color: "var(--text-lo)", "font-size": "var(--md-label-m-size)", "font-weight": "var(--md-label-m-weight)" }}><span>SESSIONS</span><Icon name="search" size="sm" /></div>
      <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)", margin: "var(--md-space-3) 0", color: "var(--text-hi)", "font-size": "var(--md-body-s-size)" }}><StatusDot status="running" /><span>roost · main</span></div>
      <div style={{ color: "var(--text-lo)", "font-size": "var(--md-label-s-size)" }}>~/projects/roost</div>
    </aside>
    <main class="workbench-editor-region" style={{ display: "grid", "grid-template-rows": "var(--workbench-tab-strip-height) minmax(0, 1fr)", "min-width": 0 }}>
    <div class="df-tab-bar" aria-label="Workbench tabs">
      <div class="df-tab workbench-pane-tab" data-testid="tab-roost-main" data-active="true">
        <Button variant="text" class="workbench-pane-tab__select" aria-label="Select roost main">
          <Icon name="terminal" size="sm" />
          roost · main
        </Button>
        <IconButton icon="close" label="Close roost main" class="df-tab-close workbench-pane-tab__close" />
      </div>
      <div class="df-tab workbench-pane-tab" data-testid="tab-roost-logs" data-active="false" data-focused="true">
        <Button variant="text" class="workbench-pane-tab__select" aria-label="Select worker logs">
          <Icon name="description" size="sm" />
          worker logs
        </Button>
        <IconButton icon="close" label="Close worker logs" class="df-tab-close workbench-pane-tab__close" />
      </div>
      <Surface
        level={2}
        elevation={3}
        radius="sm"
        class="df-menu-enter workbench-tab-list"
        style={{ position: "static" }}
      >
        <div class="workbench-tab-list__filter">
          <Icon name="search" class="workbench-tab-list__filter-icon" size="sm" />
          <input class="workbench-tab-list__input" aria-label="Filter terminals in this pane" placeholder="Filter terminals" />
        </div>
        <div class="workbench-tab-list__items" role="menu" aria-label="Open terminals in this pane">
          <Button variant="text" class="workbench-tab-list__item df-menu-item" role="menuitem" data-selected="true">
            <Icon name="terminal" class="workbench-tab-list__item-icon" size="sm" />
            <span class="workbench-tab-list__item-label">roost · main</span>
          </Button>
          <Button variant="text" class="workbench-tab-list__item df-menu-item" role="menuitem">
            <Icon name="description" class="workbench-tab-list__item-icon" size="sm" />
            <span class="workbench-tab-list__item-label">worker logs</span>
          </Button>
        </div>
      </Surface>
    </div>
      <div style={{ padding: "var(--md-space-4)", color: "var(--terminal-grid-fg)", "font-family": "var(--term-font-family)", "font-size": "var(--md-body-s-size)" }}>$ roost status</div>
    </main>
    <footer class="workbench-status-bar" style={{ "grid-column": "1 / -1", display: "flex", "align-items": "center", gap: "var(--md-space-3)", padding: "0 var(--md-space-3)", background: "var(--workbench-status)", color: "var(--text-mid)", "font-size": "var(--md-label-s-size)" }}>
      <StatusDot status="ok" /><span>Synced</span><span>1 session</span><span>worker online</span>
    </footer>
  </Surface>
);

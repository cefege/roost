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
    <header
      class="workbench-titlebar"
      style={{
        "grid-column": "1 / -1",
        display: "grid",
        "grid-template-columns": "minmax(0, 1fr) auto",
        "align-items": "center",
        padding: "0 var(--md-space-3)",
        background: "var(--workbench-titlebar)",
      }}
    >
      <div class="workbench-titlebar__left" style={{ display: "flex", "align-items": "center", gap: "var(--md-space-3)" }}>
        <Icon name="terminal" filled />
        <span style={{ "font-size": "var(--md-title-s-size)", "font-weight": "var(--md-title-s-weight)" }}>Roost</span>
        <span style={{ color: "var(--text-lo)", "font-size": "var(--md-label-m-size)" }}>Sessions</span>
      </div>
      <div class="workbench-titlebar__right" style={{ display: "flex", "justify-content": "flex-end" }}>
        <a class="workbench-titlebar__help" href="/help" aria-label="Help" title="Help">
          <Icon name="help" />
        </a>
      </div>
    </header>
    <nav class="workbench-activity-bar" aria-label="Workbench activity" style={{ display: "flex", "flex-direction": "column", "align-items": "center", gap: "var(--md-space-2)", padding: "var(--md-space-2)", background: "var(--workbench-activity)" }}>
      <Icon style={{ color: "var(--workbench-active)" }} name="terminal" filled />
      <Icon name="search" />
      <Icon name="folder" />
      <span style={{ flex: 1 }} />
      <Icon name="settings" />
    </nav>
    <aside class="workbench-sidebar-region workbench-sidebar" style={{ display: "flex", "flex-direction": "column", padding: "var(--md-space-3)", background: "var(--workbench-sidebar)" }}>
      <div style={{ display: "flex", "justify-content": "space-between", color: "var(--text-lo)", "font-size": "var(--md-label-m-size)", "font-weight": "var(--md-label-m-weight)" }}><span>SPACES</span><Icon name="search" size="sm" /></div>
      <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)", margin: "var(--md-space-3) 0", color: "var(--text-hi)", "font-size": "var(--md-body-s-size)" }}><StatusDot status="running" /><span>roost · main</span></div>
      <div style={{ color: "var(--text-lo)", "font-size": "var(--md-label-s-size)" }}>~/projects/roost</div>
      <div style={{ "border-block-start": "var(--workbench-border-width) solid var(--workbench-sidebar-border)", margin: "var(--md-space-3) 0" }} />
      <div style={{ color: "var(--text-lo)", "font-size": "var(--md-label-m-size)", "font-weight": "var(--md-label-m-weight)" }}>AGENTS</div>
      <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)", margin: "var(--md-space-2) 0", color: "var(--text-hi)", "font-size": "var(--md-body-s-size)" }}><StatusDot status="running" /><span>omp · working</span></div>
      <div style={{ color: "var(--text-lo)", "font-size": "var(--md-label-s-size)" }}>roost · main</div>
    </aside>
    <main class="workbench-editor-region" style={{ display: "grid", "grid-template-rows": "var(--workbench-tab-strip-height) minmax(0, 1fr)", "min-width": 0 }}>
      <div class="workbench-pane-tab-strip" aria-label="Workbench tabs">
        <div class="df-tab-bar workbench-pane-tab-strip__tabs">
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
          <div class="df-tab-filler workbench-pane-tab-strip__filler" />
        </div>
        <div class="workbench-pane-tab-strip__actions" role="toolbar" aria-label="Terminal actions">
          <IconButton icon="keyboard_arrow_down" label="All terminals in this pane" class="df-tab-overflow" />
          <IconButton icon="add" label="New terminal" class="df-tab-new" />
        </div>
      </div>
      <div style={{ padding: "var(--md-space-4)", color: "var(--terminal-grid-fg)", "font-family": "var(--term-font-family)", "font-size": "var(--md-body-s-size)" }}>$ roost status</div>
    </main>
    <footer class="workbench-status-bar" style={{ "grid-column": "1 / -1", display: "flex", "align-items": "center", gap: "var(--md-space-3)", padding: "0 var(--md-space-3)", background: "var(--workbench-status)", color: "var(--text-mid)", "font-size": "var(--md-label-s-size)" }}>
      <StatusDot status="ok" /><span>Synced</span><span>1 session</span><span>worker online</span>
    </footer>
  </Surface>
);

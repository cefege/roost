// DesignGallery — the single visual reference for the Roost design system.
// Renders the canonical workbench shell, color tokens, type, density scales,
// and live examples of every shared settings primitive on one /design page.
//
// HARD RULE: colors + font-sizes come ONLY from theme tokens via var(--…).
// No raw hex / rgb() / px font-size (ratcheted by scripts/lint-roost.ts).
//
// Owner: routes.ts ROUTES.DESIGN → App.tsx <Route>. Depends on:
//   ./Settings/md/primitives.tsx + theme-vars.css.

import { type JSX, type Component, For } from "solid-js";
import {
  Button, IconButton, Card, SectionTitle, List, ListRow, MetricTile, EmptyState,
  Surface, StatusDot, Icon,
} from "./Settings/md/primitives";
import { WorkbenchShellSpecimen } from "./WorkbenchShellSpecimen.tsx";
import { SettingsNavigationSpecimen } from "./SettingsNavigationSpecimen.tsx";
import { DesignControlStates } from "./DesignControlStates.tsx";
import { DesignOverlayStates } from "./DesignOverlayStates.tsx";

// ─── token catalogs (grep-tokens; each maps 1:1 to a declared theme var) ─────
const COLOR_GROUPS: { title: string; tokens: string[] }[] = [
  { title: "Surfaces", tokens: ["--surface-0", "--surface-1", "--surface-2", "--surface-3", "--bg-base", "--term-bg"] },
  { title: "Text", tokens: ["--text-hi", "--text-mid", "--text-lo"] },
  { title: "Accent", tokens: ["--accent", "--brand-coral", "--accent-container", "--on-accent"] },
  { title: "M3 roles", tokens: ["--md-primary", "--md-primary-container", "--md-secondary-container", "--md-surface-container", "--md-surface-container-high", "--md-outline", "--md-outline-variant"] },
  { title: "Status", tokens: ["--status-ok", "--status-warn", "--status-err", "--status-info"] },
  { title: "ANSI", tokens: [
    "--ansi-black", "--ansi-red", "--ansi-green", "--ansi-yellow", "--ansi-blue", "--ansi-magenta", "--ansi-cyan", "--ansi-white",
    "--ansi-bright-black", "--ansi-bright-red", "--ansi-bright-green", "--ansi-bright-yellow", "--ansi-bright-blue", "--ansi-bright-magenta", "--ansi-bright-cyan", "--ansi-bright-white",
  ] },
];

const RAMP_STEPS = [
  "display-l", "display-m", "display-s",
  "headline-l", "headline-m", "headline-s",
  "title-l", "title-m", "title-s",
  "body-l", "body-m", "body-s",
  "label-l", "label-m", "label-s",
] as const;

const SPACE_STEPS = [1, 2, 3, 4, 5, 6, 7, 8, 9] as const;
const SHAPE_STEPS = ["xs", "sm", "md", "lg", "xl", "full"] as const;
const ELEV_STEPS = [0, 1, 2, 3, 4, 5] as const;
const STATUS_DOTS = ["ok", "running", "idle", "error", "offline"] as const;

// ─── leaf helpers ────────────────────────────────────────────────────────────
const Swatch: Component<{ token: string }> = (props) => (
  <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-1)", "min-width": 0 }}>
    <div style={{
      height: "calc(var(--md-space-7) + var(--md-space-6))",
      background: `var(${props.token})`,
      "border-radius": "var(--md-shape-sm)",
      border: "var(--workbench-border-width) solid var(--md-outline-variant)",
    }} />
    <span style={{
      "font-size": "var(--md-label-s-size)",
      "line-height": "var(--md-label-s-line)",
      color: "var(--text-mid)",
      "font-family": "var(--font-mono)",
      "overflow-wrap": "anywhere",
    }}>{props.token}</span>
  </div>
);

const RampRow: Component<{ step: string }> = (props) => (
  <div style={{ display: "flex", "align-items": "baseline", gap: "var(--md-space-4)", padding: "var(--md-space-2) 0", "border-bottom": "var(--workbench-border-width) solid var(--md-outline-variant)" }}>
    <span style={{
      "flex-shrink": 0, width: "calc(var(--md-space-9) * 2 + var(--md-space-3))",
      "font-size": "var(--md-label-m-size)", "line-height": "var(--md-label-m-line)",
      color: "var(--text-lo)", "font-family": "var(--font-mono)",
    }}>{props.step}</span>
    <span style={{
      color: "var(--text-hi)",
      "font-size": `var(--md-${props.step}-size)`,
      "line-height": `var(--md-${props.step}-line)`,
      "font-weight": `var(--md-${props.step}-weight)`,
      "min-width": 0, "overflow-wrap": "anywhere",
    }}>Roost — one tab, your whole fleet</span>
  </div>
);

const SectionHeader: Component<{ children: JSX.Element }> = (props) => (
  <h2 style={{
    color: "var(--text-hi)", margin: "0 0 var(--md-space-4)",
    "font-size": "var(--md-title-l-size)",
    "line-height": "var(--md-title-l-line)",
    "font-weight": "var(--md-title-l-weight)",
  }}>{props.children}</h2>
);

const Section: Component<{ title: string; children: JSX.Element }> = (props) => (
  <Surface level={1} elevation={1} radius="lg" pad={6} border style={{ display: "block", "margin-bottom": "var(--md-space-6)" }}>
    <SectionHeader>{props.title}</SectionHeader>
    {props.children}
  </Surface>
);

const grid = (min: string): JSX.CSSProperties => ({
  display: "grid",
  "grid-template-columns": `repeat(auto-fill, minmax(${min}, 1fr))`,
  gap: "var(--md-space-4)",
});


// ─── page ────────────────────────────────────────────────────────────────────
export const DesignGallery: Component = () => {
  return (
    <div style={{
      "min-height": "100vh", "overflow-y": "auto",
      background: "var(--bg-base)", color: "var(--text-hi)",
      padding: "var(--md-space-6)", "box-sizing": "border-box",
    }}>
      <header style={{ "margin-bottom": "var(--md-space-6)" }}>
        <h1 style={{
          margin: 0, color: "var(--text-hi)",
          "font-size": "var(--md-display-s-size)",
          "line-height": "var(--md-display-s-line)",
          "font-weight": "var(--md-display-s-weight)",
        }}>Design system</h1>
        <p style={{
          margin: "var(--md-space-2) 0 0", color: "var(--text-mid)",
          "font-size": "var(--md-body-m-size)", "line-height": "var(--md-body-m-line)",
        }}>Every token + primitive on one page. Colors + type are token-only.</p>
      </header>

      <Section title="Canonical workbench shell">
        <p style={{ margin: "0 0 var(--md-space-4)", color: "var(--text-mid)", "font-size": "var(--md-body-s-size)", "line-height": "var(--md-body-s-line)" }}>
          Title bar, activity rail, primary sidebar, terminal editor, and truthful status bar share one contiguous desktop surface.
        </p>
        <WorkbenchShellSpecimen />
      </Section>

      <Section title="Settings navigation">
        <SettingsNavigationSpecimen />
      </Section>

      {/* 1. Color roles */}
      <Section title="Color roles">
        <For each={COLOR_GROUPS}>
          {(group) => (
            <div style={{ "margin-bottom": "var(--md-space-5)" }}>
              <SectionTitle>{group.title}</SectionTitle>
              <div style={grid("calc(var(--md-space-9) * 2 + var(--md-space-6))")}>
                <For each={group.tokens}>{(t) => <Swatch token={t} />}</For>
              </div>
            </div>
          )}
        </For>
      </Section>

      {/* 2. Type ramp */}
      <Section title="Type ramp">
        <For each={RAMP_STEPS}>{(step) => <RampRow step={step} />}</For>
      </Section>

      {/* 3. Spacing scale */}
      <Section title="Spacing scale">
        <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}>
          <For each={SPACE_STEPS}>
            {(n) => (
              <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-3)" }}>
                <span style={{
                  width: "calc(var(--md-space-9) * 2)", "flex-shrink": 0, color: "var(--text-lo)",
                  "font-size": "var(--md-label-m-size)", "line-height": "var(--md-label-m-line)",
                  "font-family": "var(--font-mono)",
                }}>--md-space-{n}</span>
                <div style={{
                  height: "var(--md-space-4)", width: `var(--md-space-${n})`,
                  background: "var(--md-primary)", "border-radius": "var(--md-shape-xs)",
                }} />
              </div>
            )}
          </For>
        </div>
      </Section>

      {/* 4. Shape / radii */}
      <Section title="Shape / radii">
        <div style={grid("calc(var(--md-space-9) * 2 + var(--md-space-6))")}>
          <For each={SHAPE_STEPS}>
            {(s) => (
              <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)", "align-items": "center" }}>
                <div style={{
                  width: "calc(var(--md-space-9) * 2 - var(--md-space-2))", height: "calc(var(--md-space-9) * 2 - var(--md-space-2))",
                  background: "var(--md-primary-container)",
                  border: "var(--workbench-border-width) solid var(--md-outline)",
                  "border-radius": `var(--md-shape-${s})`,
                }} />
                <span style={{
                  color: "var(--text-mid)", "font-family": "var(--font-mono)",
                  "font-size": "var(--md-label-s-size)", "line-height": "var(--md-label-s-line)",
                }}>--md-shape-{s}</span>
              </div>
            )}
          </For>
        </div>
      </Section>

      {/* 5. Elevation */}
      <Section title="Elevation">
        <div style={grid("calc(var(--md-space-9) * 3)")}>
          <For each={ELEV_STEPS}>
            {(n) => (
              <div style={{
                height: "calc(var(--md-space-9) * 2 - var(--md-space-2))", display: "flex", "align-items": "center", "justify-content": "center",
                background: "var(--surface-2)", "border-radius": "var(--md-shape-md)",
                "box-shadow": `var(--md-elev-${n})`,
                color: "var(--text-mid)", "font-family": "var(--font-mono)",
                "font-size": "var(--md-label-s-size)", "line-height": "var(--md-label-s-line)",
              }}>--md-elev-{n}</div>
            )}
          </For>
        </div>
      </Section>

      <Section title="Control states">
        <DesignControlStates />
      </Section>

      <Section title="Content primitives">
        <SectionTitle>Cards</SectionTitle>
        <div style={{ ...grid("calc(var(--md-space-9) * 5)"), "margin-bottom": "var(--md-space-5)" }}>
          <Card title="Default card" supporting="default variant">
            <span style={{ color: "var(--text-mid)", "font-size": "var(--md-body-s-size)", "line-height": "var(--md-body-s-line)" }}>Body content.</span>
          </Card>
          <Card variant="elevated" title="Elevated card" supporting="variant=elevated" trailing={<IconButton icon="more_vert" label="More" />}>
            <span style={{ color: "var(--text-mid)", "font-size": "var(--md-body-s-size)", "line-height": "var(--md-body-s-line)" }}>Body content.</span>
          </Card>
          <Card variant="outlined" title="Outlined card" supporting="variant=outlined">
            <span style={{ color: "var(--text-mid)", "font-size": "var(--md-body-s-size)", "line-height": "var(--md-body-s-line)" }}>Body content.</span>
          </Card>
        </div>

        <SectionTitle>List</SectionTitle>
        <List contained class="" >
          <ListRow leading={<Icon name="terminal" />} headline="Static row" support="non-interactive" trailing={<StatusDot status="idle" />} />
          <ListRow leading="folder" headline="Clickable row" support="onClick set" onClick={() => {}} trailing={<Icon name="chevron_right" />} />
          <ListRow leading={<Icon name="check_circle" />} headline="Selected row" support="selected=true" selected onClick={() => {}} trailing={<StatusDot status="ok" />} />
        </List>
        <div style={{ height: "var(--md-space-5)" }} />

        <SectionTitle>Metric tiles</SectionTitle>
        <div style={{ ...grid("calc(var(--md-space-9) * 4)"), "margin-bottom": "var(--md-space-5)" }}>
          <MetricTile label="CPU" icon="memory" value="42%" support="8 cores" ratio={0.42} />
          <MetricTile label="Memory" icon="memory" value="11.3 GB" support="of 16 GB" ratio={0.71} />
          <MetricTile label="Disk" icon="storage" value="220 GB" support="of 512 GB" ratio={0.43} />
        </div>

        <SectionTitle>Empty state</SectionTitle>
        <div style={{ "margin-bottom": "var(--md-space-5)" }}>
          <EmptyState icon="inbox" title="Nothing here yet" supporting="Empty-state primitive with an icon, title, supporting text, and an action." action={<Button variant="secondary" icon="add">Create</Button>} />
        </div>

        <SectionTitle>Surface (level / elevation / radius / pad)</SectionTitle>
        <div style={{ ...grid("calc(var(--md-space-9) * 4)"), "margin-bottom": "var(--md-space-5)" }}>
          <Surface level={2} elevation={0} radius="sm" pad={4} border>
            <span style={{ "font-size": "var(--md-label-m-size)", "line-height": "var(--md-label-m-line)", color: "var(--text-mid)" }}>level=2 elev=0 sm pad=4 border</span>
          </Surface>
          <Surface level={3} elevation={2} radius="lg" pad={5}>
            <span style={{ "font-size": "var(--md-label-m-size)", "line-height": "var(--md-label-m-line)", color: "var(--text-mid)" }}>level=3 elev=2 lg pad=5</span>
          </Surface>
          <Surface level={0} elevation={4} radius="xl" pad={6} border>
            <span style={{ "font-size": "var(--md-label-m-size)", "line-height": "var(--md-label-m-line)", color: "var(--text-mid)" }}>level=0 elev=4 xl pad=6 border</span>
          </Surface>
        </div>

        <SectionTitle>Status dots (solid + hollow)</SectionTitle>
        <div style={{ display: "flex", gap: "var(--md-space-5)", "flex-wrap": "wrap", "margin-bottom": "var(--md-space-5)" }}>
          <For each={STATUS_DOTS}>
            {(s) => (
              <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)", "align-items": "center" }}>
                <div style={{ display: "flex", gap: "var(--md-space-2)" }}>
                  <StatusDot status={s} size={12} title={s} />
                  <StatusDot status={s} size={12} hollow title={`${s} hollow`} />
                </div>
                <span style={{ color: "var(--text-lo)", "font-size": "var(--md-label-s-size)", "line-height": "var(--md-label-s-line)", "font-family": "var(--font-mono)" }}>{s}</span>
              </div>
            )}
          </For>
        </div>

      </Section>

      <Section title="Overlay states">
        <DesignOverlayStates />
      </Section>
    </div>
  );
};

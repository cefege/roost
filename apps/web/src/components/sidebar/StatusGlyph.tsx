// StatusGlyph — 16×16 colored box + SVG icon keyed on AgentStatus.
// Single source of truth for running/needs-input/idle/done indicator.
// Bg: peach=needs-input, green=running, surface2=idle/done.
// Claude rows show the mascot: spinner=running, ClaudeMarkSleeping (Zzz)
// =idle, ClaudeMark=otherwise. The glyph is meant to carry the state on
// its own so the status text can be dropped.
// Callers: SessionRow.
// Depends on: @roost/shared/wire AgentStatus + .roost-zzz keyframes (sidebar.css).

import type { Component } from "solid-js";
import { createSignal, onCleanup, onMount } from "solid-js";
import type { AgentStatus } from "@roost/shared/wire";
import { isPageVisible } from "../../lib/pageVisible.ts";

interface Props {
  status: AgentStatus | undefined;
  /** True when a claude TUI is detected in the terminal grid (scraped
   *  status != "unknown" OR session.agent.kind === "claude"). When set,
   *  the icon is replaced with Anthropic's 8-spoked asterisk mark in
   *  the brand coral so the row is recognisable at a glance —
   *  agent identity beats generic terminal indicator. */
  isClaude?: boolean;
}

function bgForStatus(s: AgentStatus | undefined): string {
  if (s === "needs-input") return "var(--peach)";
  if (s === "running") return "var(--green)";
  return "var(--surface2)";
}

function fgForStatus(s: AgentStatus | undefined): string {
  if (s === "needs-input" || s === "running") return "var(--base)";
  return "var(--text)";
}

export const StatusGlyph: Component<Props> = (props) => {
  return (
    <span
      data-testid="status-glyph"
      data-status={props.status ?? "idle"}
      data-is-claude={props.isClaude ? "1" : "0"}
      class="shrink-0 inline-flex items-center justify-center"
      style={{
        // Explicit flex-center: the utility classes above are dead (no
        // Tailwind/Uno in this app), so without this the sub-16px glyphs
        // (running spinner char, needs-input/done SVGs, idle `$`) sit at
        // the box's top-left instead of centered.
        display: "inline-flex",
        "align-items": "center",
        "justify-content": "center",
        width: "16px",
        height: "16px",
        "border-radius": "var(--radius-sm, 3px)",
        background: props.isClaude ? "transparent" : bgForStatus(props.status),
        color: props.isClaude
          ? "var(--term-color-9)"
          : fgForStatus(props.status),
      }}
      aria-label={props.isClaude ? "claude" : `status ${props.status ?? "idle"}`}
    >
      {props.isClaude
        ? (props.status === "running" ? <ClaudeSpinner />
            : (props.status === "idle" || props.status === undefined) ? <ClaudeMarkSleeping />
            : <ClaudeMark />)
        : <StatusIcon status={props.status} />}
    </span>
  );
};

/** Cycling claude TUI spinner glyphs — the exact rotation claude uses
 *  in its own status line. ONE shared interval drives all spinners on
 *  the page; ref-counted so the timer only runs while at least one
 *  ClaudeSpinner is mounted. Replaces the per-instance setInterval that
 *  scaled N timers with N running-claude rows AND drifted out of phase
 *  between rows. */
const SPINNER_FRAMES = ["✻", "✶", "✽", "✦"];
const [_spinnerIdx, _setSpinnerIdx] = createSignal(0);
let _spinnerRefs = 0;
let _spinnerHandle: ReturnType<typeof setInterval> | null = null;

function _acquireSpinnerTick(): void {
  _spinnerRefs++;
  if (_spinnerHandle !== null) return;
  _spinnerHandle = setInterval(
    () => { if (isPageVisible()) _setSpinnerIdx((i) => (i + 1) % SPINNER_FRAMES.length); },
    160,
  );
}
function _releaseSpinnerTick(): void {
  _spinnerRefs--;
  if (_spinnerRefs > 0 || _spinnerHandle === null) return;
  clearInterval(_spinnerHandle);
  _spinnerHandle = null;
}

const ClaudeSpinner: Component = () => {
  onMount(() => {
    _acquireSpinnerTick();
    onCleanup(_releaseSpinnerTick);
  });
  return (
    <span
      aria-hidden="true"
      style={{
        "font-size": "14px",
        "line-height": "1",
        "font-family": "var(--term-font-family, 'JBM Nerd', monospace)",
      }}
    >
      {SPINNER_FRAMES[_spinnerIdx()]}
    </span>
  );
};

/** Official Claude Code mascot — the pixel-art "robot" mark in the
 *  Anthropic brand coral (#D97757). Path lifted verbatim from the
 *  shipping claude-code.svg so the icon stays canonical instead of a
 *  hand-rolled approximation. EXPORTED so the TabBar can use the same
 *  glyph for claude tabs — single source of truth, no drift. */
export const ClaudeMark: Component = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
    <path
      clip-rule="evenodd"
      d="M20.998 10.949H24v3.102h-3v3.028h-1.487V20H18v-2.921h-1.487V20H15v-2.921H9V20H7.488v-2.921H6V20H4.487v-2.921H3V14.05H0V10.95h3V5h17.998v5.949zM6 10.949h1.488V8.102H6v2.847zm10.51 0H18V8.102h-1.49v2.847z"
      fill="currentColor"
      fill-rule="evenodd"
    />
  </svg>
);

/** Idle claude = the same mascot, asleep. Mark dimmed + two `z`s drifting
 *  up off its head (CSS float in sidebar.css; static under reduced-motion).
 *  Goal: the glyph alone reads the agent state so the status text can go
 *  away. Shares ClaudeMark's path so the robot never drifts from canonical. */
const ClaudeMarkSleeping: Component = () => (
  <span class="roost-sleeping">
    <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
      <path
        clip-rule="evenodd"
        d="M20.998 10.949H24v3.102h-3v3.028h-1.487V20H18v-2.921h-1.487V20H15v-2.921H9V20H7.488v-2.921H6V20H4.487v-2.921H3V14.05H0V10.95h3V5h17.998v5.949zM6 10.949h1.488V8.102H6v2.847zm10.51 0H18V8.102h-1.49v2.847z"
        fill="currentColor"
        fill-rule="evenodd"
        opacity="0.55"
      />
    </svg>
    <span class="roost-zzz" aria-hidden="true">
      <span class="roost-zzz-1">z</span>
      <span class="roost-zzz-2">z</span>
    </span>
  </span>
);

const StatusIcon: Component<{ status: AgentStatus | undefined }> = (props) => {
  if (props.status === "needs-input") {
    return (
      <svg
        width="10"
        height="10"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="2.25"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <line x1="8" y1="3" x2="8" y2="9" />
        <line x1="8" y1="12" x2="8" y2="13" />
      </svg>
    );
  }
  if (props.status === "running") {
    return (
      <svg
        width="10"
        height="10"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="1.75"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <circle cx="8" cy="8" r="5.5" />
        <path d="M8 2.5 A5.5 5.5 0 0 1 13.5 8 L8 8 Z" fill="currentColor" stroke="none" />
      </svg>
    );
  }
  if (props.status === "done") {
    return (
      <svg
        width="10"
        height="10"
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        stroke-width="2.25"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M3.5 8.5 L6.5 11.5 L12.5 5" />
      </svg>
    );
  }
  // idle / undefined → "$" prompt char, matching the TabBar's shell
  // glyph (Author 2026-06-18: "sidebar should have the terminal icon
  // the same as the terminal icon on the tabs"). Single source of
  // truth across both surfaces; no SVG drift.
  return (
    <span
      aria-hidden="true"
      style={{
        "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace",
        "font-size": "11px",
        "font-weight": 600,
        "line-height": 1,
      }}
    >$</span>
  );
};

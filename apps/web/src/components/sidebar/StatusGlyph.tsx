// StatusGlyph — 16×16 colored indicator keyed on normalized terminal state.
import type { Component } from "solid-js";

type OmpStatus = "needs-input" | "running" | "idle" | "done";

interface Props {
  status: OmpStatus | undefined;
}

function bgForStatus(s: OmpStatus | undefined): string {
  if (s === "needs-input") return "var(--peach)";
  if (s === "running") return "var(--green)";
  return "var(--surface2)";
}

function fgForStatus(s: OmpStatus | undefined): string {
  if (s === "needs-input" || s === "running") return "var(--base)";
  return "var(--text)";
}

export const StatusGlyph: Component<Props> = (props) => (
  <span
    data-testid="status-glyph"
    data-status={props.status ?? "idle"}
    class="shrink-0 inline-flex items-center justify-center"
    style={{
      display: "inline-flex",
      "align-items": "center",
      "justify-content": "center",
      width: "16px",
      height: "16px",
      "border-radius": "var(--radius-sm, 3px)",
      background: bgForStatus(props.status),
      color: fgForStatus(props.status),
    }}
    aria-label={`status ${props.status ?? "idle"}`}
  >
    <StatusIcon status={props.status} />
  </span>
);

const StatusIcon: Component<{ status: OmpStatus | undefined }> = (props) => {
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

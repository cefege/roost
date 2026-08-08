// Contextual empty-state panel. Picks copy + icon based on kind:
//   coord-error | browser-unpaired | no-machines | search-empty | view-empty
// Called by AllView, SwarmView, InboxView when their content list is empty.
// Depends on: useNavigate for the CTA navigation.

import { Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { Button } from "../Settings/md/primitives.tsx";

export type EmptyStateKind =
  | "coord-error"
  | "browser-unpaired"
  | "no-machines"
  | "search-empty"
  | "view-empty";

interface SidebarEmptyStateProps {
  kind: EmptyStateKind;
  query?: string;
  coordError?: string | null;
}

export function SidebarEmptyState(props: SidebarEmptyStateProps) {
  const navigate = useNavigate();

  const copy = () => pickCopy(props.kind, props.query ?? "", props.coordError ?? null);

  const ctaLabel = (): string | null => {
    if (props.kind === "coord-error") return "Open Settings";
    if (props.kind === "browser-unpaired") return "Pair this browser";
    if (props.kind === "no-machines") return "Add a Machine";
    return null;
  };

  const ctaHref = (): string => {
    // browser-unpaired → Onboarding (tap-to-pair + bootstrap token).
    // no-machines     → worker-Mac pairing pane (a different problem:
    //                   browser IS trusted, but no worker has registered).
    // coord-error     → settings landing (machines is the default pane).
    if (props.kind === "browser-unpaired") return "/pair";
    return "/settings/machines";
  };

  const iconColor = () =>
    props.kind === "coord-error" ? "var(--color-err)" : "var(--text-lo)";

  return (
    <div
      data-testid="sidebar-empty-state"
      data-kind={props.kind}
      style={{
        display: "flex",
        "flex-direction": "column",
        "align-items": "center",
        gap: "12px",
        padding: "32px 16px",
        "text-align": "center",
        color: "var(--text-lo)",
      }}
    >
      {/* Icon */}
      <div
        aria-hidden="true"
        style={{ width: "40px", height: "40px", color: iconColor(), display: "flex", "align-items": "center", "justify-content": "center" }}
      >
        <Show when={props.kind === "coord-error"}>
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/>
            <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>
            <line x1="2" y1="2" x2="22" y2="22"/>
          </svg>
        </Show>
        <Show when={props.kind === "browser-unpaired"}>
          {/* Key glyph — "this browser needs a key to read coord data". */}
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="8" cy="15" r="4"/>
            <line x1="10.85" y1="12.15" x2="19" y2="4"/>
            <line x1="18" y1="5" x2="20" y2="7"/>
            <line x1="15" y1="8" x2="17" y2="10"/>
          </svg>
        </Show>
        <Show when={props.kind === "no-machines"}>
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <rect x="2" y="3" width="20" height="14" rx="2" ry="2"/>
            <line x1="8" y1="21" x2="16" y2="21"/>
            <line x1="12" y1="17" x2="12" y2="21"/>
            <line x1="2" y1="2" x2="22" y2="22"/>
          </svg>
        </Show>
        <Show when={props.kind === "search-empty"}>
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8"/>
            <path d="m21 21-4.35-4.35"/>
            <line x1="8" y1="8" x2="14" y2="14"/>
            <line x1="14" y1="8" x2="8" y2="14"/>
          </svg>
        </Show>
        <Show when={props.kind === "view-empty"}>
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="22 12 16 12 14 15 10 15 8 12 2 12"/>
            <path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>
          </svg>
        </Show>
      </div>

      {/* Title */}
      <div style={{ "font-size": "13px", color: "var(--text-hi)", "font-weight": 500 }}>
        {copy().title}
      </div>

      {/* Body */}
      <div style={{ "font-size": "12px", color: "var(--text-lo)", "line-height": 1.5 }}>
        {copy().body}
      </div>

      {/* CTA */}
      <Show when={ctaLabel()}>
        {(label) => (
          <Button
            variant="tonal"
            data-testid="sidebar-empty-state-cta"
            onClick={() => navigate(ctaHref())}
          >
            {label()}
          </Button>
        )}
      </Show>
    </div>
  );
}

function pickCopy(
  kind: EmptyStateKind,
  query: string,
  coordError: string | null,
): { title: string; body: string } {
  if (kind === "coord-error") {
    return {
      title: "Coordinator unreachable",
      body: coordError ?? "Roost can't reach the coordinator. Check the URL in Settings.",
    };
  }
  if (kind === "browser-unpaired") {
    return {
      title: "This browser isn't paired",
      body: "Pair this browser with the coordinator to see your machines, workspaces, and sessions.",
    };
  }
  if (kind === "no-machines") {
    return {
      title: "No machines registered",
      body: "Connect a Mac or Linux machine as a worker to start a terminal here.",
    };
  }
  if (kind === "search-empty") {
    return {
      title: "No matches",
      body: `Nothing matches "${query}". Esc clears the search.`,
    };
  }
  return {
    title: "Nothing here yet",
    body: "Switch to the All chip to see every workspace.",
  };
}

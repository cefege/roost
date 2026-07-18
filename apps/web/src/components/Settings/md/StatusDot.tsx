import { type Component } from "solid-js";

// ─── StatusDot — the ONE status indicator (design-system phase 1) ───────────
// Replaces the 3 divergent implementations (AppShell inline #4ade80, SessionRow
// --term-color-*, Settings --status-*). status → a canonical status token.
const STATUS_DOT_TOKEN: Record<string, string> = {
  ok: "--status-ok",
  done: "--status-ok",
  running: "--md-primary",
  "running-workflow": "--md-primary",
  "needs-input": "--status-warn",
  idle: "--text-lo",
  offline: "--text-lo",
  error: "--status-err",
  info: "--status-info",
};
export const StatusDot: Component<{
  status: string;
  size?: number;      // px diameter (default 8)
  hollow?: boolean;   // outline-only (idle/done in the folder list)
  title?: string;
}> = (props) => {
  const token = () => `var(${STATUS_DOT_TOKEN[props.status] ?? "--text-lo"})`;
  const d = () => `${props.size ?? 8}px`;
  return (
    <span
      aria-hidden="true"
      title={props.title}
      style={{
        display: "inline-block",
        "flex-shrink": 0,
        width: d(),
        height: d(),
        "border-radius": "50%",
        "box-sizing": "border-box",
        ...(props.hollow
          ? { background: "transparent", border: `1.5px solid ${token()}` }
          : { background: token() }),
      }}
    />
  );
};

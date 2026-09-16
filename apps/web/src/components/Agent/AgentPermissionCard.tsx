// Mecatl's permission ask, rendered as the pane's one blocking decision.
// Allow once / Allow always / Deny map straight onto the server's verdict
// vocabulary — Roost holds no approval model of its own and stores no verdict.
//
// Verdicts are only offered for a run this pane started: the SDK refuses
// resolveAsk on a durable attachment, so a followed run's ask is informational.

import { Show } from "solid-js";
import type { PermissionVerdict } from "@stacklok-oss/mecatl-sdk";
import { Button, Card, StatusDot } from "../Settings/md/primitives.tsx";
import type { AgentAskEntry, AgentAskState } from "./agentTimeline.ts";

const RESOLVED_LABELS: Record<string, string | undefined> = {
  allow_once: "Allowed once",
  allow_always: "Allowed always",
  deny: "Denied",
  retracted: "Withdrawn",
};

export function AgentPermissionCard(props: {
  entry: AgentAskEntry;
  actionable: boolean;
  onVerdict: (verdict: PermissionVerdict) => void;
}) {
  const pending = () => props.entry.state === "pending";
  const resolvedLabel = (state: AgentAskState) => RESOLVED_LABELS[state] ?? "Resolved";

  return (
    <Card
      variant="elevated"
      data-testid={`agent-ask-${props.entry.askId}`}
      style={{
        display: "flex",
        "flex-direction": "column",
        gap: "var(--md-space-3)",
        border: "1px solid var(--md-sys-color-primary)",
      }}
    >
      <div style={{ display: "flex", "align-items": "center", gap: "var(--md-space-2)" }}>
        <StatusDot status={pending() ? "warn" : "idle"} title="Permission" />
        <span class="md-title-s">Allow {props.entry.tool}?</span>
      </div>

      <Show when={props.entry.reason !== ""}>
        <p
          class="md-body-m"
          style={{ margin: 0, color: "var(--md-sys-color-on-surface-variant)" }}
        >
          {props.entry.reason}
        </p>
      </Show>

      <Show when={props.entry.args !== ""}>
        <pre
          class="md-body-s"
          style={{
            margin: 0,
            "max-height": "calc(var(--md-space-9) * 3)",
            overflow: "auto",
            "white-space": "pre-wrap",
            "overflow-wrap": "anywhere",
            color: "var(--md-sys-color-on-surface-variant)",
            "font-family": "var(--font-mono)",
          }}
        >
          {props.entry.args}
        </pre>
      </Show>

      <Show
        when={pending() && props.actionable}
        fallback={
          <span class="md-label-m" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>
            {pending()
              ? "Answer this where the run was started."
              : resolvedLabel(props.entry.state)}
          </span>
        }
      >
        <div style={{ display: "flex", gap: "var(--md-space-2)", "flex-wrap": "wrap" }}>
          <Button
            variant="default"
            data-testid="agent-ask-allow-once"
            onClick={() => props.onVerdict("allow_once")}
          >
            Allow once
          </Button>
          <Button
            variant="secondary"
            data-testid="agent-ask-allow-always"
            onClick={() => props.onVerdict("allow_always")}
          >
            Allow always
          </Button>
          <Button
            variant="destructive"
            data-testid="agent-ask-deny"
            onClick={() => props.onVerdict("deny")}
          >
            Deny
          </Button>
        </div>
      </Show>
    </Card>
  );
}

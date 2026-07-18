// McpRelayEditor — inline form row for creating a new MCP relay.
// Renders a two-field form (label + target path/URL) with submit/cancel.
// Depends on: md/primitives (TextField, Button). Callers: McpPane.tsx.

import { createSignal, Show } from "solid-js";
import { TextField, Button, Select } from "./Settings/md/primitives.tsx";

interface McpRelayEditorProps {
  onSave: (label: string, kind: "stdio" | "sse", config: Record<string, unknown>) => Promise<void>;
  onCancel: () => void;
}

export function McpRelayEditor(props: McpRelayEditorProps) {
  const [label, setLabel] = createSignal("");
  const [target, setTarget] = createSignal("");
  const [kind, setKind] = createSignal<"stdio" | "sse">("stdio");
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal("");

  const canSubmit = () => label().trim().length > 0 && target().trim().length > 0 && !saving();

  async function handleSave() {
    if (!canSubmit()) return;
    setSaving(true);
    setError("");
    try {
      // Build config from kind + target.
      // stdio → { command: target }; sse → { url: target }.
      const config: Record<string, unknown> =
        kind() === "stdio" ? { command: target().trim() } : { url: target().trim() };
      await props.onSave(label().trim(), kind(), config);
      setSaving(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  }

  return (
    <div
      data-testid="mcp-relay-editor"
      style={{
        background: "var(--bg-elev-1)",
        border: "1px solid var(--md-sys-color-outline-variant)",
        "border-radius": "var(--md-shape-sm)",
        padding: "14px 16px",
        display: "flex",
        "flex-direction": "column",
        gap: "10px",
      }}
    >
      <TextField
        testId="mcp-editor-label"
        label="Relay name"
        value={label()}
        onInput={(v) => setLabel(v)}
        placeholder="e.g. filesystem"
      />

      <Select
        testId="mcp-editor-kind"
        label="Kind"
        value={kind()}
        onChange={(v) => setKind(v as "stdio" | "sse")}
        options={[
          { value: "stdio", label: "stdio (binary path)" },
          { value: "sse", label: "sse (URL)" },
        ]}
      />

      <TextField
        testId="mcp-editor-target"
        label={kind() === "stdio" ? "Binary path" : "Server URL"}
        value={target()}
        onInput={(v) => setTarget(v)}
        placeholder={kind() === "stdio" ? "/usr/local/bin/mcp-server" : "https://mcp.example.com/sse"}
      />

      <Show when={error()}>
        <p style={{ color: "var(--status-err)", "font-size": "12px", margin: 0 }}>{error()}</p>
      </Show>

      <div style={{ display: "flex", gap: "8px" }}>
        <Button
          variant="filled"
          data-testid="mcp-editor-save"
          onClick={() => void handleSave()}
          disabled={!canSubmit()}
        >
          {saving() ? "Adding…" : "Add relay"}
        </Button>
        <Button
          variant="text"
          data-testid="mcp-editor-cancel"
          onClick={props.onCancel}
        >
          Cancel
        </Button>
      </div>
    </div>
  );
}

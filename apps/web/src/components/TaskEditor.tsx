// TaskEditor: full-form editor for enqueuing a Task.
// Fields: body (prompt), cwd, worker_fp pick, priority.
// Callers: QueueTaskDialog (modal), QueueView (inline "new task" row).
// Depends on: coordClient (tasksEnqueue), rootStore.workers, M3 primitives.

import { createSignal, Show } from "solid-js";
import type { Component } from "solid-js";
import { coordClient } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { TextField, Select, Button } from "./Settings/md/primitives.tsx";


const MONO: Record<string, string> = { "--md-outlined-field-content-font": "ui-monospace, monospace" };

// ─── types ─────────────────────────────────────────────────────────────────

export interface TaskEditorProps {
  /** Pre-fill values. */
  defaultBody?: string;
  defaultCwd?: string;
  defaultWorkerFp?: string;
  /** Called after successful enqueue. */
  onEnqueued?: () => void;
  /** Called on cancel. */
  onCancel?: () => void;
  /** If false, Cancel button is hidden (inline mode). */
  showCancel?: boolean;
}

// ─── component ─────────────────────────────────────────────────────────────

export const TaskEditor: Component<TaskEditorProps> = (props) => {
  const [body, setBody] = createSignal(props.defaultBody ?? "");
  const [cwd, setCwd] = createSignal(props.defaultCwd ?? "");
  const [workerFp, setWorkerFp] = createSignal(props.defaultWorkerFp ?? "");
  const [priority, setPriority] = createSignal(0);
  const [completionCheck, setCompletionCheck] = createSignal("");
  const [submitting, setSubmitting] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const workers = () => Object.values(rootStore.workers);
  const workerOptions = () => [
    { value: "", label: "— any worker —" },
    ...workers().map((w) => ({ value: w.fp, label: w.label })),
  ];

  const handleSubmit = async () => {
    const b = body().trim();
    if (!b) { setError("body required"); return; }
    setSubmitting(true);
    setError(null);
    try {
      await coordClient.tasksEnqueue({
        payloadJson: JSON.stringify({
          body: b,
          cwd: cwd().trim() || undefined,
          worker_fp: workerFp() || undefined,
          priority: priority(),
        }),
        completionCheck: completionCheck().trim() || undefined,
      });
      props.onEnqueued?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); props.onCancel?.(); }
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void handleSubmit(); }
  };

  return (
    <div
      data-testid="task-editor"
      onKeyDown={onKeyDown}
      style={{ display: "flex", "flex-direction": "column", gap: "12px" }}
    >
      <TextField
        testId="task-editor-body"
        label="Prompt"
        type="textarea"
        rows={4}
        value={body()}
        onInput={setBody}
        placeholder="Describe what the agent should do…"
      />

      <TextField
        testId="task-editor-cwd"
        label="Working directory"
        value={cwd()}
        onInput={setCwd}
        placeholder="/Users/you/code/repo"
        style={MONO}
      />

      <Show when={workers().length > 0}>
        <Select
          testId="task-editor-worker"
          label="Worker (optional)"
          value={workerFp()}
          onChange={setWorkerFp}
          options={workerOptions()}
        />
      </Show>


      <TextField
        testId="task-editor-priority"
        label="Priority"
        type="number"
        value={String(priority())}
        onInput={(v) => setPriority(Number(v))}
        min={-100}
        max={100}
        style={{ width: "120px" }}
      />

      <TextField
        testId="task-editor-completion-check"
        label="Completion check (shell cmd, optional)"
        value={completionCheck()}
        onInput={setCompletionCheck}
        placeholder="gh pr view --json state -q .state | grep MERGED"
        style={MONO}
      />

      <Show when={error()}>
        <div data-testid="task-editor-error" style={{ color: "var(--color-err)", "font-size": "11px" }}>
          {error()}
        </div>
      </Show>

      <div style={{ display: "flex", gap: "8px", "justify-content": "flex-end" }}>
        <Show when={props.showCancel !== false && props.onCancel}>
          <Button variant="text" onClick={props.onCancel}>Cancel</Button>
        </Show>
        <Button
          variant="filled"
          data-testid="task-editor-submit"
          disabled={submitting()}
          onClick={() => void handleSubmit()}
        >
          {submitting() ? "Queuing…" : "Queue  ⌘↩"}
        </Button>
      </div>
    </div>
  );
};

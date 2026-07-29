// A durable snapshot of omp's todo phases. The payload is JSON because the
// todo schema belongs to omp; malformed or newer shapes degrade to no panel
// instead of taking down the transcript.

import { For, Show, createMemo, type Component } from "solid-js";
import { Surface } from "../Settings/md/Surface.tsx";
import type { AgentTodoEntry } from "@roost/shared/wire/agent-entry";

type TodoTask = {
  content: string;
  status: string;
  blocker?: string;
};

type TodoPhase = {
  name: string;
  tasks: TodoTask[];
};

type RawTodoTask = {
  content?: unknown;
  status?: unknown;
  blocker?: unknown;
};

type RawTodoPhase = {
  name?: unknown;
  tasks?: unknown;
};

type StatusPresentation = {
  glyph: string;
  label: string;
  color: string;
};

export const EntryTodo: Component<{ entry: AgentTodoEntry }> = (props) => {
  const phases = createMemo(() => parsePhases(props.entry.phases_json));

  return (
    <Show when={phases().length > 0}>
      <Surface
        level={1}
        radius="md"
        pad={3}
        border
        style={{ "max-width": "min(46rem, 88%)", color: "var(--md-on-surface)" }}
      >
        <div
          data-testid="agent-entry-todo"
          data-seq={props.entry.seq}
          aria-label="Todo list"
          style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-3)" }}
        >
          <For each={phases()}>
            {(phase) => (
              <section aria-label={phase.name}>
                <div
                  style={{
                    "font-size": "var(--md-label-l-size)",
                    "line-height": "var(--md-label-l-line)",
                    "font-weight": "var(--md-label-l-weight)",
                  }}
                >
                  {phase.name}
                </div>
                <Show when={phase.tasks.length > 0}>
                  <ul
                    style={{
                      display: "flex",
                      "flex-direction": "column",
                      gap: "var(--md-space-1)",
                      margin: "var(--md-space-2) 0 0",
                      padding: "0",
                      "list-style": "none",
                    }}
                  >
                    <For each={phase.tasks}>
                      {(task) => {
                        const status = statusPresentation(task.status);
                        return (
                          <li
                            data-status={task.status}
                            style={{
                              display: "flex",
                              "align-items": "baseline",
                              gap: "var(--md-space-2)",
                              color: "var(--md-on-surface)",
                              "font-size": "var(--md-body-m-size)",
                              "line-height": "var(--md-body-m-line)",
                            }}
                          >
                            <span
                              role="img"
                              aria-label={status.label}
                              title={status.label}
                              style={{
                                width: "var(--md-space-4)",
                                "flex-shrink": 0,
                                color: status.color,
                                "text-align": "center",
                              }}
                            >
                              {status.glyph}
                            </span>
                            <div style={{ "min-width": 0 }}>
                              <span style={{ "overflow-wrap": "anywhere" }}>{task.content}</span>
                              <Show when={task.blocker}>
                                {(blocker) => (
                                  <div
                                    style={{
                                      color: "var(--status-warn)",
                                      "font-size": "var(--md-label-s-size)",
                                      "line-height": "var(--md-label-s-line)",
                                      "overflow-wrap": "anywhere",
                                    }}
                                  >
                                    Blocked: {blocker()}
                                  </div>
                                )}
                              </Show>
                            </div>
                          </li>
                        );
                      }}
                    </For>
                  </ul>
                </Show>
              </section>
            )}
          </For>
        </div>
      </Surface>
    </Show>
  );
};

function parsePhases(raw: string): TodoPhase[] {
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];

    const phases: TodoPhase[] = [];
    for (const value of parsed) {
      if (typeof value !== "object" || value === null || Array.isArray(value)) continue;
      const phase = value as RawTodoPhase;

      const tasks: TodoTask[] = [];
      if (Array.isArray(phase.tasks)) {
        for (const candidate of phase.tasks) {
          if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) continue;
          const task = candidate as RawTodoTask;
          if (typeof task.content !== "string" || !task.content.trim()) continue;
          tasks.push({
            content: task.content,
            status: typeof task.status === "string" && task.status ? task.status : "pending",
            blocker: typeof task.blocker === "string" && task.blocker.trim() ? task.blocker : undefined,
          });
        }
      }

      const name = typeof phase.name === "string" ? phase.name.trim() : "";
      if (!name && tasks.length === 0) continue;
      phases.push({ name: name || "Tasks", tasks });
    }
    return phases;
  } catch {
    return [];
  }
}

function statusPresentation(status: string): StatusPresentation {
  switch (status) {
    case "completed":
      return { glyph: "✓", label: "Completed", color: "var(--status-ok)" };
    case "in_progress":
      return { glyph: "▸", label: "In progress", color: "var(--md-primary)" };
    case "cancelled":
      return { glyph: "✗", label: "Cancelled", color: "var(--status-err)" };
    case "abandoned":
      return { glyph: "✗", label: "Abandoned", color: "var(--status-err)" };
    case "pending":
      return { glyph: "·", label: "Pending", color: "var(--md-on-surface-dim)" };
    case "blocked":
      return { glyph: "!", label: "Blocked", color: "var(--status-warn)" };
    default:
      return { glyph: "·", label: status || "Unknown status", color: "var(--md-on-surface-dim)" };
  }
}

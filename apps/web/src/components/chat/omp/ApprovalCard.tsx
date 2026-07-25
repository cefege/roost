// ApprovalCard — inline approval for the native RPC chat. omp asks via
// extension_ui_request; the worker surfaces it as an `approval` ContentBlock,
// and answering tunnels an extension_ui_response back over the SAME transport
// the composer uses. No terminal round trip.
//
// Once resolved the card renders the decision as static text: the block stays
// in the transcript, so re-answering must be impossible.

import { createEffect, createSignal, For, on, Show } from "solid-js";
import type { ApprovalBlock } from "@roost/shared/chat/wire";
import { coordClient } from "../../../connect.ts";
import { addToast } from "../../../lib/toastStore.ts";
import { Button } from "../../Settings/md/Button.tsx";
import { TextField } from "../../Settings/md/TextField.tsx";
import { Icon } from "../../Settings/md/Icon.tsx";

interface Props {
  sessionId: string;
  block: ApprovalBlock;
}

export function ApprovalCard(props: Props) {
  const [busy, setBusy] = createSignal(false);
  // `editor` is omp's free-text branch (the ask tool's "Other (type your own)");
  // it reuses the input card and its optional prefill rides in `message`.
  // `input` puts its PLACEHOLDER there instead, so only editor seeds the field.
  const isText = () => props.block.method === "input" || props.block.method === "editor";
  const [text, setText] = createSignal(props.block.method === "editor" ? props.block.message : "");

  const answer = async (reply: Record<string, unknown>) => {
    if (busy()) return;
    setBusy(true);
    try {
      await coordClient.sessionsChatCommand({
        sessionId: props.sessionId,
        commandJson: JSON.stringify({ type: "extension_ui_response", id: props.block.requestId, ...reply }),
      });
    } catch (e) {
      addToast(`Approval failed: ${e instanceof Error ? e.message : String(e)}`, "err");
      setBusy(false);
    }
  };

  // A multi-select's toggle chain reuses ONE chat message id, and the store
  // reconciles by id — so this component is PATCHED, never remounted, when omp
  // re-prompts with the boxes ticked. Without resetting on the new request id
  // the first tick leaves every row disabled and the card is dead.
  createEffect(on(() => props.block.requestId, () => setBusy(false), { defer: true }));

  // Roles the option list renders as rows; the rest ride the footer as actions.
  const rows = () => props.block.richOptions.filter((c) => c.role === "option" || c.role === "other");
  const byRole = (role: string) => props.block.richOptions.find((c) => c.role === role);

  return (
    <div class="omp-approval" data-testid="omp-chat-approval" data-method={props.block.method}>
      <div>
        {/* Resolved is history, not a live demand — the :has() demotion below
            drops the fill, and the glyph has to follow or the row keeps
            shouting after it was answered. */}
        <Icon name={props.block.resolved ? "check" : "warning"} size="sm" class="omp-approval__icon" />
        <Show when={props.block.header}>
          <span class="omp-approval__chip">{props.block.header}</span>
        </Show>
        {props.block.title || "approval needed"}
        <Show when={props.block.progress}>
          <span class="omp-approval__progress">{props.block.progress}</span>
        </Show>
        <Show when={props.block.message && props.block.method !== "editor"}>
          <div class="omp-approval__msg">{props.block.message}</div>
        </Show>
      </div>
      <Show
        when={!props.block.resolved}
        fallback={<div class="omp-approval__answer">→ {props.block.answer}</div>}
      >
        <Show when={props.block.method === "confirm"}>
          <div class="omp-approval__actions">
            <Button variant="filled" data-testid="omp-chat-approval-approve" disabled={busy()} onClick={() => void answer({ confirmed: true })}>Approve</Button>
            <Button variant="text" data-testid="omp-chat-approval-deny" disabled={busy()} onClick={() => void answer({ confirmed: false })}>Deny</Button>
          </div>
        </Show>
        <Show when={props.block.method === "select"}>
          {/* Rich path: one row per option with its marker and description.
              Falls back to the flat button row when the worker sent no render
              model (version skew), so a card never comes up empty. */}
          <Show
            when={rows().length > 0}
            fallback={
              <div class="omp-approval__actions">
                <For each={props.block.options}>
                  {(opt) => (
                    <Button variant="tonal" data-testid="omp-chat-approval-option" disabled={busy()} onClick={() => void answer({ value: opt })}>{opt}</Button>
                  )}
                </For>
                <Button variant="text" data-testid="omp-chat-approval-dismiss" disabled={busy()} onClick={() => void answer({ cancelled: true })}>Dismiss</Button>
              </div>
            }
          >
            <div class="omp-approval__options" data-testid="omp-chat-approval-options">
              <For each={rows()}>
                {(choice) => (
                  <button
                    type="button"
                    class="omp-approval__option"
                    data-testid="omp-chat-approval-option"
                    data-role={choice.role}
                    data-checked={String(choice.checked)}
                    disabled={busy()}
                    onClick={() => void answer({ value: choice.value })}
                  >
                    <span class="omp-approval__marker">
                      {choice.role === "other" ? "✎" : props.block.multi ? (choice.checked ? "☑" : "☐") : choice.checked ? "◉" : "○"}
                    </span>
                    <span class="omp-approval__copy">
                      <span class="omp-approval__label">
                        {choice.label}
                        <Show when={choice.recommended}>
                          <span class="omp-approval__rec">Recommended</span>
                        </Show>
                      </span>
                      <Show when={choice.description}>
                        <span class="omp-approval__desc">{choice.description}</span>
                      </Show>
                    </span>
                  </button>
                )}
              </For>
            </div>
            <div class="omp-approval__actions">
              <Show when={byRole("done")}>
                {(done) => (
                  <Button variant="filled" data-testid="omp-chat-approval-done" disabled={busy()} onClick={() => void answer({ value: done().value })}>Done selecting</Button>
                )}
              </Show>
              <Show when={byRole("next")}>
                {(next) => (
                  <Button variant="tonal" data-testid="omp-chat-approval-next" disabled={busy()} onClick={() => void answer({ value: next().value })}>Next →</Button>
                )}
              </Show>
              <Show when={byRole("back")}>
                {(back) => (
                  <Button variant="text" data-testid="omp-chat-approval-back" disabled={busy()} onClick={() => void answer({ value: back().value })}>← Back</Button>
                )}
              </Show>
              <Button variant="text" data-testid="omp-chat-approval-dismiss" disabled={busy()} onClick={() => void answer({ cancelled: true })}>Dismiss</Button>
            </div>
          </Show>
        </Show>
        <Show when={isText()}>
          <div class="omp-approval__actions">
            <TextField
              class="omp-approval__input"
              label="Your answer"
              value={text()}
              disabled={busy()}
              onInput={setText}
              testId="omp-chat-approval-input"
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); void answer({ value: text() }); }
              }}
            />
            <Button variant="filled" data-testid="omp-chat-approval-submit" disabled={busy()} onClick={() => void answer({ value: text() })}>Submit</Button>
            <Button variant="text" data-testid="omp-chat-approval-dismiss" disabled={busy()} onClick={() => void answer({ cancelled: true })}>Dismiss</Button>
          </div>
        </Show>
      </Show>
    </div>
  );
}

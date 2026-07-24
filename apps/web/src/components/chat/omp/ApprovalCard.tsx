// ApprovalCard — inline approval for the native RPC chat. omp asks via
// extension_ui_request; the worker surfaces it as an `approval` ContentBlock,
// and answering tunnels an extension_ui_response back over the SAME transport
// the composer uses. No terminal round trip.
//
// Once resolved the card renders the decision as static text: the block stays
// in the transcript, so re-answering must be impossible.

import { createSignal, For, Show } from "solid-js";
import type { ApprovalBlock } from "@roost/shared/chat/wire";
import { coordClient } from "../../../connect.ts";
import { addToast } from "../../../lib/toastStore.ts";

interface Props {
  sessionId: string;
  block: ApprovalBlock;
}

export function ApprovalCard(props: Props) {
  const [busy, setBusy] = createSignal(false);
  const [text, setText] = createSignal("");

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

  return (
    <div class="omp-approval" data-testid="omp-chat-approval">
      <div>
        ⚠ {props.block.title || "approval needed"}
        <Show when={props.block.message}>
          <div class="omp-approval__msg">{props.block.message}</div>
        </Show>
      </div>
      <Show
        when={!props.block.resolved}
        fallback={<div class="omp-approval__answer">→ {props.block.answer}</div>}
      >
        <Show when={props.block.method === "confirm"}>
          <div class="omp-approval__actions">
            <button class="omp-approval__btn" disabled={busy()} onClick={() => void answer({ confirmed: true })}>Approve</button>
            <button class="omp-approval__btn" disabled={busy()} onClick={() => void answer({ confirmed: false })}>Deny</button>
          </div>
        </Show>
        <Show when={props.block.method === "select"}>
          <div class="omp-approval__actions">
            <For each={props.block.options}>
              {(opt) => (
                <button class="omp-approval__btn" disabled={busy()} onClick={() => void answer({ value: opt })}>{opt}</button>
              )}
            </For>
            <button class="omp-approval__btn" disabled={busy()} onClick={() => void answer({ cancelled: true })}>Dismiss</button>
          </div>
        </Show>
        <Show when={props.block.method === "input"}>
          <div class="omp-approval__actions">
            <input
              class="omp-approval__input"
              value={text()}
              disabled={busy()}
              onInput={(e) => setText(e.currentTarget.value)}
              onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); void answer({ value: text() }); } }}
            />
            <button class="omp-approval__btn" disabled={busy()} onClick={() => void answer({ value: text() })}>Submit</button>
            <button class="omp-approval__btn" disabled={busy()} onClick={() => void answer({ cancelled: true })}>Dismiss</button>
          </div>
        </Show>
      </Show>
    </div>
  );
}

// ApprovalCard — bounded in-chat approval. omp's scrape sets status:"blocked"
// but does NOT populate permission_request with options, so v1 renders a
// "switch to terminal" prompt (one-tap). Full Approve/Deny buttons land when a
// future scrape extracts tool-name + options into permission_request.

import { Show } from "solid-js";
import { toggleOmpChatView } from "../../../store/uiStore.ts";

interface Props {
  sessionId: string;
  blocked: boolean;
  toolName?: string;
}

export function ApprovalCard(props: Props) {
  return (
    <Show when={props.blocked}>
      <div class="omp-approval" data-testid="omp-chat-approval">
        <div>
          ⚠ approval needed
          <Show when={props.toolName}>: {props.toolName}</Show>
        </div>
        <div class="omp-approval__actions">
          <button class="omp-approval__btn" onClick={() => toggleOmpChatView(props.sessionId)}>
            Switch to terminal
          </button>
        </div>
      </div>
    </Show>
  );
}

// MessageBubble — renders one ChatMessage's text/image blocks. toolCall/toolResult
// blocks are NOT rendered here (OmpChatPane routes them to ToolCard); this handles
// user (right bubble), assistant/developer (left bubble), and images.

import { For, Show } from "solid-js";
import type { ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import { renderMarkdown } from "./markdown.ts";

interface Props {
  msg: ChatMessage;
}

/** Non-tool blocks for a message (text/image/thinking). Tool blocks render in ToolCard. */
function proseBlocks(msg: ChatMessage): ContentBlock[] {
  return msg.blocks.filter((b) => b.kind === "text" || b.kind === "image");
}

export function MessageBubble(props: Props) {
  const blocks = () => proseBlocks(props.msg);
  const isUser = () => props.msg.role === "user";

  return (
    <Show when={blocks().length > 0}>
      <div class={`omp-msg omp-msg--${props.msg.role}`} data-testid="omp-chat-msg">
        <div class="omp-msg__bubble">
          <For each={blocks()}>
            {(b) => (
              <Show when={b.kind === "text"} fallback={<ImageBlock blobPath={b.kind === "image" ? b.blobPath : ""} mime={b.kind === "image" ? b.mime : ""} />}>
                <div class="omp-md" innerHTML={renderMarkdown((b as { text: string }).text)} />
              </Show>
            )}
          </For>
        </div>
      </div>
    </Show>
  );
}

function ImageBlock(props: { blobPath: string; mime: string }) {
  // blobPath is either a data: URL (inline base64) or an absolute worker path.
  // data: URLs render directly; absolute paths need a byte fetch (v1: placeholder).
  const isDataUrl = () => props.blobPath.startsWith("data:");
  return (
    <Show when={isDataUrl()} fallback={<span class="omp-md" style={{ color: "var(--md-on-surface-dim)" }}>[image: {props.mime}]</span>}>
      <img src={props.blobPath} alt="" style={{ "max-width": "100%", "border-radius": "var(--md-shape-sm)", margin: "var(--md-space-1) 0" }} />
    </Show>
  );
}

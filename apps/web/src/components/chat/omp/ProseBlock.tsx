// ProseBlock — one text or image block as transcript body content: omp's tr-md
// markdown block, no bubble chrome (the gutter row in OmpChatPane names the
// speaker). One block per instance, so the node survives a streaming message
// growing — a <For> here would rebuild the row and re-parse its markdown on
// every 60ms frame. toolCall/toolResult blocks route to ToolCard instead.

import { Show } from "solid-js";
import type { TextBlock, ImageBlock } from "@roost/shared/chat/wire";
import { renderMarkdown } from "./markdown.ts";
import { ChatImage } from "./ChatImage.tsx";

interface Props {
  block: TextBlock | ImageBlock;
  sessionId: string;
  /** Parity-oracle stamp: the JSON TuiRow this element paints (see
   *  @roost/shared/chat/rows). Undefined on elements that anchor no row. */
  dataTuiRow?: string;
}

export function ProseBlock(props: Props) {
  const text = () => (props.block.kind === "text" ? props.block : null);
  const img = () => (props.block.kind === "image" ? props.block : null);

  return (
    <Show when={text()} fallback={
      // ChatImage, not a local <img>: blobPath is a data: URL OR an absolute
      // worker path, and only ChatImage can fetch the latter (chunked
      // filesReadChunk → data URL). Rendering it here meant every image omp
      // persisted to its blob store painted as an inert [image: mime] marker.
      <Show when={img()}>
        {(im) => <ChatImage sessionId={props.sessionId} blobPath={im().blobPath} mime={im().mime} dataTuiRow={props.dataTuiRow} />}
      </Show>
    }>
      {(t) => <div class="tr-md" data-testid="omp-chat-msg" data-tui-row={props.dataTuiRow} innerHTML={renderMarkdown(t().text)} />}
    </Show>
  );
}

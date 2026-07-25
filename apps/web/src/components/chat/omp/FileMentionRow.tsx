// FileMentionRow — the `@path` files attached to a prompt, as the same list of
// `Read <path>` rows omp's buildFileMentionBlock paints
// (transcript-render-helpers.ts:104-125). omp additionally suffixes each row
// with a line count or a skip reason; the wire carries paths only, so the
// suffix is absent on both sides of a Roost pane rather than faked here.

import { For } from "solid-js";
import type { FileMentionBlock } from "@roost/shared/chat/wire";

export function FileMentionRow(props: { block: FileMentionBlock }) {
  return (
    <div class="tr-mentions">
      <For each={props.block.paths}>
        {(path) => (
          <div class="tr-mention" data-testid="omp-chat-file-mention">
            <span class="tr-mention-verb">Read</span>
            <span class="tr-mention-path">{path}</span>
          </div>
        )}
      </For>
    </div>
  );
}

// NoticeRow — the turn-ending line omp appends under an assistant message when
// the turn aborted, errored, or recovered from a retry
// (AssistantMessageComponent.updateContent, assistant-message.ts:855-870; the
// text itself comes from resolveAssistantErrorPresentation, ported worker-side).
//
// PLAIN TEXT on purpose: the TUI prints this through `Text`, not `Markdown`, so
// an error string carrying backticks, asterisks or a stray `#` must survive
// verbatim instead of being eaten by a markdown pass.

import type { NoticeBlock } from "@roost/shared/chat/wire";

export function NoticeRow(props: { block: NoticeBlock; dataTuiRow?: string }) {
  return (
    <div class="tr-notice" data-testid="omp-chat-notice" data-level={props.block.level} data-tui-row={props.dataTuiRow}>
      {props.block.text}
    </div>
  );
}

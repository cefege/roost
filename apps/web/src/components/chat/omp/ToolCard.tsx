// ToolCard — mounts omp's own <omp-tool-view> custom element for one tool call,
// so a read/edit/bash/todo renders exactly as it does in omp's web transcript
// (per-tool card, diff, checklist, grep hits). The element is registered by the
// vendored bundle (src/vendor/omp-tool-views.js) and takes its whole payload on
// the `data` property. Images stay ours: omp cannot resolve Roost blob paths.

import { createMemo, createEffect, Show, For } from "solid-js";
import type { ToolCallBlock, ToolEventBlock, ImageBlock } from "@roost/shared/chat/wire";
import { safeJsonParse } from "@roost/shared/json";
import type { ResultRef } from "./renderPlan.ts";
import { ChatImage } from "./ChatImage.tsx";

interface Props {
  sessionId: string;
  call?: ToolCallBlock | null;
  results?: ResultRef[];
  event?: ToolEventBlock | null;
  images?: ImageBlock[];
}

/** ToolViewProps as omp's element reads it. `args`/`result` are deliberately
 *  loose — the renderers tolerate partial and malformed payloads by design. */
interface ToolViewPayload {
  name: string;
  args: Record<string, unknown>;
  result?: unknown;
  running: boolean;
  intent?: string;
  partial?: string;
  defaultOpen: boolean;
}

/** Port of omp's parseMCPToolName (src/mcp/tool-bridge.ts:366-379) + the label it
 *  builds at tool-bridge.ts:413. omp's browser tool-view bundle has no mcp__
 *  awareness, so the wire name would print raw. */
function displayToolName(name: string): string {
  if (!name.startsWith("mcp__")) return name;
  const rest = name.slice(5);
  const i = rest.indexOf("_");
  if (i === -1) return name;
  return `${rest.slice(0, i)}/${rest.slice(i + 1)}`;
}

export function ToolCard(props: Props) {
  const results = () => props.results ?? [];
  const name = () => props.call?.name ?? results()[0]?.block.name ?? props.event?.name ?? "tool";
  // Only a live tool_execution event can mean "running": a call with no event
  // and no result is history (trimmed/compacted), and must not spin forever.
  const running = () => (props.event ? props.event.phase !== "end" && results().length === 0 : false);

  const payload = createMemo<ToolViewPayload>(() => {
    const raw = results()[0]?.block.rawJson;
    return {
      name: displayToolName(name()),
      args: safeJsonParse<Record<string, unknown>>(props.call?.argsJson, {}, "chat.toolCall.args"),
      result: raw ? safeJsonParse<unknown>(raw, undefined, "chat.toolResult.rawJson") : undefined,
      running: running(),
      intent: props.event?.intent || undefined,
      // Live output omp streams while the tool runs. Superseded by the result.
      partial: running() ? props.event?.output || undefined : undefined,
      defaultOpen: false,
    };
  });

  let el!: HTMLElement & { data?: ToolViewPayload };
  // The element only re-renders on the property setter, so the assignment IS
  // the update path — an attribute or a JSX prop would paint once and freeze.
  createEffect(() => { el.data = payload(); });

  return (
    <div class="omp-tool" data-testid="omp-chat-tool">
      <omp-tool-view ref={el} class="tv-host" />
      <Show when={props.images && props.images.length > 0}>
        <div class="omp-tool__images">
          <For each={props.images}>
            {(im) => <ChatImage sessionId={props.sessionId} blobPath={im.blobPath} mime={im.mime} />}
          </For>
        </div>
      </Show>
    </div>
  );
}

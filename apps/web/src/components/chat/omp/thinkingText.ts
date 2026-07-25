// Thinking-block preparation, ported from omp so Roost's pane renders reasoning
// the way omp does: dots-only blocks dropped, adjacent blocks read as one
// continuous passage.

import type { ContentBlock, ThinkingBlock as ThinkingData } from "@roost/shared/chat/wire";

/** One thinking block inside a run. `index` is its position in the owning
 *  message's block list — the lazy full-text fetch's coordinate. */
export interface ThinkingPart {
  block: ThinkingData;
  index: number;
}

/** A message's blocks after grouping. */
export type Rendered =
  | { kind: "block"; block: ContentBlock; index: number }
  | { kind: "thinking"; parts: ThinkingPart[]; index: number };

/** Port of omp's canonicalizeMessage (src/utils/thinking-display.ts:15-25):
 *  a thinking block that is only dots/ellipses/whitespace carries nothing and
 *  is not rendered. */
export function canonicalizeThinking(text: string | null | undefined): string {
  if (!text) return "";
  const trimmed = text.trim();
  for (let i = 0; i < trimmed.length; i++) {
    const code = trimmed.charCodeAt(i);
    if (code !== 0x2e && code !== 0x2026 && code !== 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) {
      return trimmed;
    }
  }
  return "";
}

/** Collapse each maximal run of adjacent thinking blocks into one passage, as
 *  omp renders consecutive reasoning (assistant-message.ts:799-844: unlabeled
 *  blocks reading as one continuous italic run). A run is emitted at the
 *  position of its first surviving member, so ordering is preserved. */
export function groupThinking(blocks: readonly ContentBlock[]): Rendered[] {
  const out: Rendered[] = [];
  let run: ThinkingPart[] | null = null;
  blocks.forEach((block, index) => {
    if (block.kind === "thinking") {
      // A dots-only block is dropped but does NOT split the run: two real
      // blocks either side of it are still one passage.
      if (canonicalizeThinking(block.text) === "") return;
      if (run) run.push({ block, index });
      else {
        run = [{ block, index }];
        out.push({ kind: "thinking", parts: run, index });
      }
      return;
    }
    run = null;
    out.push({ kind: "block", block, index });
  });
  return out;
}

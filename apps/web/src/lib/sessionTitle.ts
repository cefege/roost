// Shared terminal-session naming for sidebar and tab surfaces. Prefer the
// terminal's OSC title, then a stable cwd fallback.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { shortCwd } from "./sidebarFormat.ts";
import { pathBasename } from "./pathBasename.ts";

const MAX = 80;

// Session/OSC titles are user-visible UI text, so truncation must respect
// grapheme cluster boundaries (e.g. a ZWJ family emoji), not just avoid
// splitting a surrogate pair — a plain `.slice(0, MAX)` can also cut a
// cluster in half and leave a dangling combining/ZWJ component. Segmenter
// walks whole clusters and we only ever append a full cluster, so the
// result never exceeds MAX code units (the bound the cap protects) and
// never ends mid-surrogate-pair or mid-cluster.
const titleSegmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });

function truncateTitle(text: string, max: number): string {
  if (text.length <= max) return text;
  let result = "";
  for (const { segment } of titleSegmenter.segment(text)) {
    if (result.length + segment.length > max) break;
    result += segment;
  }
  return result;
}

export function folderHeadline(session: Session): string {
  const custom = session.custom_title?.trim();
  if (custom) return truncateTitle(custom, MAX);
  return pathBasename(session.cwd, session.worker_fp) || "Terminal";
}

/** Structured program detail for an open terminal, if the process reports one. */
export function programSubtitle(session: Session): string | null {
  const osc = rootStore.terminal_title[session.id]?.trim();
  if (osc) return truncateTitle(osc, MAX);
  return null;
}

export function sessionTitle(session: Session): string {
  const custom = session.custom_title?.trim();
  if (custom) return truncateTitle(custom, MAX);
  return programSubtitle(session) || shortCwd(session.cwd, session.worker_fp) || "shell";
}

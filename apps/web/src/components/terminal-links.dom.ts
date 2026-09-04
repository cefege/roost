// Applies validated terminal links to renderer-owned row DOM without reparsing it.
// The mutation scanner supplies changed rows, and terminal-links.ts reuses the
// same anchor authoring path before a user opens a producer-painted link.
// Range guards preserve live cells when a renderer update races linkification.

import {
  computeRowLinks,
  type PaintedLink,
  type RowLinkInput,
  type RowLinkSegment,
} from "./terminal-links.detect.ts";
import {
  classifyTerminalLinkTarget,
  isWorkerFileHref,
  type ResolveFile,
  type TerminalLinkTarget,
  type WorkerFileTerminalLinkTarget,
} from "./terminal-links.target.ts";
import {
  LINK_KEY_ATTR,
  ROW_COLUMNS_ATTR,
  ROW_HAS_LINKS_ATTR,
  TERMINAL_LINK_CLASS as LINK_CLASS,
  TERMINAL_LINK_TARGET_ATTR,
} from "../lib/cellRow.ts";
import { terminalLinkModifierKey } from "../lib/browserPlatform.ts";

const SCANNED_ATTR = "data-linkified";

type PaintedAnchor = PaintedLink & { el: HTMLElement };

export function applyTerminalAnchorTarget(
  anchor: HTMLElement,
  rawTarget: string,
  target: TerminalLinkTarget,
  hint?: string,
): void {
  anchor.setAttribute(TERMINAL_LINK_TARGET_ATTR, rawTarget);
  anchor.setAttribute("tabindex", "-1");
  anchor.setAttribute("draggable", "false");
  anchor.setAttribute(
    "title",
    `${terminalLinkModifierKey() === "Meta" ? "Command" : "Control"}-click to open ${target.display}`,
  );
  if (target.kind === "external") {
    anchor.setAttribute("href", target.href);
    anchor.setAttribute("target", "_blank");
    anchor.setAttribute("rel", "noopener noreferrer");
    anchor.removeAttribute("data-kind");
    anchor.setAttribute("data-hint", hint ?? target.display);
    return;
  }
  anchor.setAttribute("data-kind", "file");
  anchor.removeAttribute("target");
  anchor.removeAttribute("rel");
  if (target.href) anchor.setAttribute("href", target.href);
  else anchor.removeAttribute("href");
  anchor.setAttribute("data-hint", hint ?? `Open ${target.display}`);
}

export function resolveTerminalAnchorTarget(
  anchor: HTMLElement,
  resolveFile: ResolveFile | undefined,
): TerminalLinkTarget | null {
  const rawTarget = anchor.getAttribute(TERMINAL_LINK_TARGET_ATTR)
    ?? anchor.getAttribute("href");
  if (!rawTarget) return null;
  const target = classifyTerminalLinkTarget(rawTarget, resolveFile);
  if (target?.kind === "file" && target.href === null) return null;
  return target;
}

// Resolve producer links before detection sees them. Invalid targets dissolve
// back to terminal text instead of blocking an inferred match at those cells.
function collectPaintedLinks(
  row: HTMLElement,
  resolveFile: ResolveFile | undefined,
): PaintedAnchor[] | undefined {
  if (!row.hasAttribute(ROW_HAS_LINKS_ATTR)) return undefined;
  const painted: PaintedAnchor[] = [];
  let offset = 0;
  for (const child of Array.from(row.childNodes)) {
    const length = (child.textContent ?? "").length;
    if (child instanceof HTMLElement && child.hasAttribute(LINK_KEY_ATTR)) {
      const rawTarget = child.getAttribute(TERMINAL_LINK_TARGET_ATTR)
        ?? child.getAttribute("href")
        ?? "";
      const target = classifyTerminalLinkTarget(rawTarget, resolveFile);
      if (target === null || (target.kind === "file" && target.href === null)) {
        child.replaceWith(...Array.from(child.childNodes));
      } else {
        applyTerminalAnchorTarget(child, rawTarget, target);
        painted.push({
          el: child,
          start: offset,
          end: offset + length,
          uri: rawTarget,
          key: child.getAttribute(LINK_KEY_ATTR) ?? "",
        });
      }
    }
    offset += length;
  }
  if (painted.length === 0) row.removeAttribute(ROW_HAS_LINKS_ATTR);
  return painted.length > 0 ? painted : undefined;
}

/** Returns painted grid occupancy, never UTF-16 text length. An unstamped row
 * cannot safely join a following row, so its sentinel never equals `cols`. */
export function terminalRowColumns(row: HTMLElement): number {
  const stamped = row.getAttribute(ROW_COLUMNS_ATTR);
  if (stamped === null) return -1;
  const columns = Number.parseInt(stamped, 10);
  return Number.isInteger(columns) ? columns : -1;
}

export function linkifyTerminalRows(
  rows: HTMLElement[],
  cols: number,
  resolveFile?: ResolveFile,
  githubOwnerRepo?: string,
): void {
  const painted: Array<PaintedAnchor[] | undefined> = [];
  const inputs: RowLinkInput[] = [];
  for (const row of rows) {
    const links = collectPaintedLinks(row, resolveFile);
    painted.push(links);
    inputs.push({ text: row.textContent ?? "", columns: terminalRowColumns(row), links });
  }
  const segments = computeRowLinks(inputs, cols, resolveFile, githubOwnerRepo);
  if (segments.length === 0) return;

  const byRow = new Map<number, RowLinkSegment[]>();
  for (const segment of segments) {
    const list = byRow.get(segment.row);
    if (list) list.push(segment);
    else byRow.set(segment.row, [segment]);
  }
  for (const [rowIdx, rowSegments] of byRow) {
    const row = rows[rowIdx];
    // Renderer updates replace row elements, so this mark identifies only an
    // unchanged row and prevents MutationObserver-driven nested anchors.
    if (row.hasAttribute(SCANNED_ATTR)) continue;
    for (const link of painted[rowIdx] ?? []) {
      if (!rowSegments.some((segment) => segment.start < link.end && link.start < segment.end)) continue;
      link.el.replaceWith(...Array.from(link.el.childNodes));
    }
    const nodes: Array<{ node: Text; start: number; end: number }> = [];
    let offset = 0;
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const textNode = node as Text;
      nodes.push({ node: textNode, start: offset, end: offset + textNode.data.length });
      offset += textNode.data.length;
    }
    for (let idx = rowSegments.length - 1; idx >= 0; idx--) {
      const segment = rowSegments[idx];
      wrapTerminalRange(
        nodes,
        segment.start,
        segment.end,
        segment.url,
        segment.kind,
        segment.hint,
        segment.source,
      );
    }
    row.setAttribute(SCANNED_ATTR, "1");
  }
}

function wrapTerminalRange(
  nodes: Array<{ node: Text; start: number; end: number }>,
  start: number,
  end: number,
  url: string,
  kind?: "file",
  hint?: string,
  source?: string,
): void {
  let target: TerminalLinkTarget | null;
  let rawTarget = url;
  if (kind === "file") {
    if (!source || !isWorkerFileHref(url)) return;
    rawTarget = source;
    target = {
      kind: "file",
      rawPath: source,
      line: null,
      href: url,
      display: source,
    } satisfies WorkerFileTerminalLinkTarget;
  } else {
    target = classifyTerminalLinkTarget(url);
    if (target?.kind !== "external") return;
  }
  const startNode = nodes.find((node) => node.end > start && node.start <= start);
  const endNode = nodes.find((node) => node.end >= end && node.start < end);
  if (!startNode || !endNode) return;
  const range = document.createRange();
  const startOffset = start - startNode.start;
  const endOffset = end - endNode.start;
  // Live renderer writes can shorten a text node between detection and range
  // creation; skipping lets the replacement row receive the next scan.
  if (startOffset < 0 || startOffset > startNode.node.length) return;
  if (endOffset < 0 || endOffset > endNode.node.length) return;
  range.setStart(startNode.node, startOffset);
  range.setEnd(endNode.node, endOffset);
  const anchor = document.createElement("a");
  anchor.className = LINK_CLASS;
  applyTerminalAnchorTarget(anchor, rawTarget, target, hint);
  try {
    if (startNode.node === endNode.node) {
      range.surroundContents(anchor);
    } else {
      anchor.appendChild(range.extractContents());
      range.insertNode(anchor);
    }
  } catch {
    // A concurrent boundary mutation is recovered by the replacement row's scan.
  }
}

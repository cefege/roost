import { markPhase } from "./diag.ts";
import type { SmokeApi } from "./smokeTypes.ts";
import {
  recordTerminalGeometryProof,
  terminalBrowserStreamSnapshot,
  type CursorPresentationProof,
  type MarkerPresentationProof,
  type TerminalRectSnapshot,
} from "./terminalDiagSnapshot.ts";

type Step = { name: string; pass: boolean; detail: unknown };

function nextFrame(): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  requestAnimationFrame(() => resolve());
  return promise;
}

async function frames(count: number): Promise<void> {
  for (let index = 0; index < count; index++) await nextFrame();
}

async function waitFor(check: () => boolean, frameLimit: number): Promise<boolean> {
  for (let frame = 0; frame < frameLimit; frame++) {
    if (check()) return true;
    await nextFrame();
  }
  return check();
}

export type PaintedMarkerProof = MarkerPresentationProof;

export type PaintedCursorProof = CursorPresentationProof;

export interface PaintedCursorExpected {
  row?: number;
  column?: number;
}

export type TerminalTimingKind = "trusted_key" | "reveal" | "resize" | "optimistic";

export type TerminalTimingResult = PaintedMarkerProof & {
  timingId: string;
  kind: TerminalTimingKind;
  startedMonotonicMs: number;
  startedEpochMs: number;
  durationMs: number;
  trustedKey: boolean;
};

type RectSnapshot = TerminalRectSnapshot;

type MarkerGeometry = {
  rowText: string;
  markerRect: RectSnapshot;
  terminalRect: RectSnapshot;
  visualViewportRect: RectSnapshot;
  rowNode: Element;
};

type CursorGeometry = {
  row: number;
  column: number;
  rawRect: RectSnapshot;
  rect: RectSnapshot;
  terminalClip: RectSnapshot;
  visualViewport: RectSnapshot;
  cursorNode: HTMLElement;
};

type PendingTiming = {
  id: string;
  kind: TerminalTimingKind;
  sessionId?: string;
  startedMonotonicMs: number | null;
  startedEpochMs: number | null;
  trustedKey: boolean;
  removeListener?: () => void;
};

const TIMING_CAPACITY = 64;
const pendingTimings = new Map<string, PendingTiming>();

function rectSnapshot(rect: DOMRect | DOMRectReadOnly): RectSnapshot {
  return {
    left: rect.left,
    top: rect.top,
    right: rect.right,
    bottom: rect.bottom,
    width: rect.width,
    height: rect.height,
  };
}

function rectanglesIntersect(left: RectSnapshot, right: RectSnapshot): boolean {
  return left.width > 0
    && left.height > 0
    && right.width > 0
    && right.height > 0
    && left.right > right.left
    && left.left < right.right
    && left.bottom > right.top
    && left.top < right.bottom;
}

function visualViewportRect(): RectSnapshot {
  const viewport = window.visualViewport;
  const left = viewport?.offsetLeft ?? 0;
  const top = viewport?.offsetTop ?? 0;
  const width = viewport?.width ?? document.documentElement.clientWidth;
  const height = viewport?.height ?? document.documentElement.clientHeight;
  return { left, top, right: left + width, bottom: top + height, width, height };
}

function hasVisibleComputedStyle(element: Element): boolean {
  let current: Element | null = element;
  while (current) {
    const style = getComputedStyle(current);
    if (
      style.display === "none"
      || style.visibility === "hidden"
      || style.visibility === "collapse"
      || Number.parseFloat(style.opacity) === 0
      || style.contentVisibility === "hidden"
    ) return false;
    current = current.parentElement;
  }
  return true;
}

function markerRange(row: Element, marker: string): Range | null {
  const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  let text = "";
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const value = node.nodeValue ?? "";
    if (value.length === 0) continue;
    nodes.push(node as Text);
    text += value;
  }
  const markerStart = text.indexOf(marker);
  if (markerStart < 0) return null;
  const markerEnd = markerStart + marker.length;
  let offset = 0;
  let startNode: Text | undefined;
  let endNode: Text | undefined;
  let startOffset = 0;
  let endOffset = 0;
  for (const node of nodes) {
    const next = offset + node.data.length;
    if (!startNode && markerStart >= offset && markerStart < next) {
      startNode = node;
      startOffset = markerStart - offset;
    }
    if (markerEnd > offset && markerEnd <= next) {
      endNode = node;
      endOffset = markerEnd - offset;
      break;
    }
    offset = next;
  }
  if (!startNode || !endNode) return null;
  const range = document.createRange();
  range.setStart(startNode, startOffset);
  range.setEnd(endNode, endOffset);
  return range;
}

function findPaintedMarker(sessionId: string, marker: string): MarkerGeometry | null {
  if (marker.length === 0) return null;
  const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
  const terminal = slot?.querySelector(".cell-grid") as HTMLElement | null;
  if (!slot || !terminal || !slot.isConnected || !terminal.isConnected) return null;
  if (!hasVisibleComputedStyle(terminal) || !hasVisibleComputedStyle(slot)) return null;
  const terminalRect = rectSnapshot(terminal.getBoundingClientRect());
  const viewportRect = visualViewportRect();
  if (!rectanglesIntersect(terminalRect, viewportRect)) return null;

  for (const row of terminal.querySelectorAll(".cell-row")) {
    if (!(row.textContent ?? "").includes(marker) || !hasVisibleComputedStyle(row)) continue;
    const range = markerRange(row, marker);
    if (!range) continue;
    const markerRect = rectSnapshot(range.getBoundingClientRect());
    const markerElement = range.startContainer.parentElement;
    if (!markerElement || !hasVisibleComputedStyle(markerElement)) continue;
    if (!rectanglesIntersect(markerRect, terminalRect) || !rectanglesIntersect(markerRect, viewportRect)) continue;
    return {
      rowText: row.textContent ?? "",
      markerRect,
      terminalRect,
      visualViewportRect: viewportRect,
      rowNode: row,
    };
  }
  return null;
}

function clippedRect(left: RectSnapshot, right: RectSnapshot): RectSnapshot | null {
  const clippedLeft = Math.max(left.left, right.left);
  const clippedTop = Math.max(left.top, right.top);
  const clippedRight = Math.min(left.right, right.right);
  const clippedBottom = Math.min(left.bottom, right.bottom);
  const width = clippedRight - clippedLeft;
  const height = clippedBottom - clippedTop;
  if (width <= 0 || height <= 0) return null;
  return {
    left: clippedLeft,
    top: clippedTop,
    right: clippedRight,
    bottom: clippedBottom,
    width,
    height,
  };
}

function cursorStyleIsPaintable(cursor: HTMLElement, terminal: HTMLElement): boolean {
  for (let current: HTMLElement | null = cursor; current; current = current.parentElement) {
    const style = getComputedStyle(current);
    if (
      style.display === "none"
      || style.visibility === "hidden"
      || style.visibility === "collapse"
      || style.contentVisibility === "hidden"
      // Cursor blink deliberately animates opacity to zero. Ancestor opacity
      // still clips presentation; the cursor's own blink phase does not.
      || (current !== cursor && Number.parseFloat(style.opacity) === 0)
    ) return false;
    if (current === cursor) {
      const background = style.backgroundColor.replace(/\s+/g, "").toLowerCase();
      if (
        background === "transparent"
        || /^rgba\([^)]*,0(?:\.0+)?\)$/.test(background)
        || /\/0(?:\.0+)?\)$/.test(background)
      ) return false;
    }
    if (current === terminal) return true;
  }
  return false;
}

function findPaintedCursor(
  sessionId: string,
  expected: PaintedCursorExpected | undefined,
): CursorGeometry | null {
  const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
  const terminal = slot?.querySelector(".cell-grid");
  const viewport = terminal?.querySelector(".cell-viewport");
  const cursor = viewport?.querySelector(".cell-cursor");
  if (
    !(slot instanceof HTMLElement)
    || !(terminal instanceof HTMLElement)
    || !(viewport instanceof HTMLElement)
    || !(cursor instanceof HTMLElement)
    || !slot.isConnected
    || !terminal.isConnected
    || !viewport.isConnected
    || !cursor.isConnected
    || cursor.parentElement !== viewport
    || !cursorStyleIsPaintable(cursor, terminal)
  ) return null;

  const row = Number(cursor.dataset.row);
  const column = Number(cursor.dataset.column);
  if (
    !Number.isSafeInteger(row)
    || row < 0
    || !Number.isSafeInteger(column)
    || column < 0
    || (expected?.row !== undefined && row !== expected.row)
    || (expected?.column !== undefined && column !== expected.column)
  ) return null;

  const terminalRect = rectSnapshot(terminal.getBoundingClientRect());
  const visualViewport = visualViewportRect();
  const terminalClip = clippedRect(terminalRect, visualViewport);
  if (!terminalClip) return null;
  const cursorRect = rectSnapshot(cursor.getBoundingClientRect());
  const cursorClip = clippedRect(cursorRect, terminalClip);
  if (!cursorClip) return null;

  const rowElements = viewport.querySelectorAll(".cell-row");
  const rowElement = rowElements.item(row);
  if (!(rowElement instanceof HTMLElement) || !hasVisibleComputedStyle(rowElement)) return null;
  const rowRect = rectSnapshot(rowElement.getBoundingClientRect());
  if (rowRect.width <= 0 || rowRect.height <= 0) return null;
  const rowTolerance = Math.max(0.75, cursorRect.height * 0.08);
  if (
    Math.abs(cursorRect.top - rowRect.top) > rowTolerance
    || cursorRect.bottom > rowRect.bottom + rowTolerance
  ) return null;
  const columnTolerance = Math.max(0.75, cursorRect.width * 0.08);
  const expectedLeft = rowRect.left + column * cursorRect.width;
  if (Math.abs(cursorRect.left - expectedLeft) > columnTolerance) return null;

  return {
    row,
    column,
    rawRect: cursorRect,
    rect: cursorClip,
    terminalClip,
    visualViewport,
    cursorNode: cursor,
  };
}

function stableRect(left: RectSnapshot, right: RectSnapshot): boolean {
  const tolerance = 0.75;
  return Math.abs(left.left - right.left) <= tolerance
    && Math.abs(left.top - right.top) <= tolerance
    && Math.abs(left.right - right.right) <= tolerance
    && Math.abs(left.bottom - right.bottom) <= tolerance
    && Math.abs(left.width - right.width) <= tolerance
    && Math.abs(left.height - right.height) <= tolerance;
}

/**
 * Cursor presentation proof: a connected cursor directly under
 * `.cell-viewport` owns stable, grid-aligned geometry clipped by both terminal
 * and visual viewport across two animation frames. This is intentionally not
 * a raster-pixel claim: the cursor must have a paintable background, while its
 * nondeterministic blink opacity is tolerated by contract.
 */
export async function waitForPaintedCursor(
  sessionId: string,
  expected?: PaintedCursorExpected,
  timeoutMs = 30_000,
): Promise<PaintedCursorProof> {
  if (
    (expected?.row !== undefined && (!Number.isSafeInteger(expected.row) || expected.row < 0))
    || (
      expected?.column !== undefined
      && (!Number.isSafeInteger(expected.column) || expected.column < 0)
    )
  ) throw new Error(`invalid expected cursor coordinates: ${JSON.stringify(expected)}`);

  const deadline = performance.now() + timeoutMs;
  while (performance.now() <= deadline) {
    const first = findPaintedCursor(sessionId, expected);
    if (first) {
      await frames(2);
      const confirmed = findPaintedCursor(sessionId, expected);
      if (
        confirmed
        && confirmed.cursorNode === first.cursorNode
        && confirmed.row === first.row
        && confirmed.column === first.column
        && stableRect(confirmed.rect, first.rect)
        && stableRect(confirmed.rawRect, first.rawRect)
        && stableRect(confirmed.terminalClip, first.terminalClip)
        && stableRect(confirmed.visualViewport, first.visualViewport)
      ) {
        const monotonicMs = performance.now();
        const proof: PaintedCursorProof = {
          proof_kind: "cursor",
          sessionId,
          row: confirmed.row,
          column: confirmed.column,
          monotonicMs,
          epochMs: performance.timeOrigin + monotonicMs,
          rect: confirmed.rect,
          terminalClip: confirmed.terminalClip,
          visualViewport: confirmed.visualViewport,
          frames: 2,
        };
        markPhase("cursor_presented", {
          sessionId,
          row: proof.row,
          column: proof.column,
          cursorWidth: proof.rect.width,
          cursorHeight: proof.rect.height,
        });
        recordTerminalGeometryProof(sessionId, proof);
        return proof;
      }
    }
    await nextFrame();
  }
  throw new Error(
    `cursor presentation geometry was not proven within ${timeoutMs}ms: ${sessionId} ${JSON.stringify(expected ?? {})}`,
  );
}

/**
 * Paint proof, not a DOM-existence probe: the exact marker must own a non-zero
 * Range rectangle inside both the terminal and visual viewport, remain visibly
 * styled, cross two animation frames, then pass the same geometry checks again.
 */
export async function waitForPaintedMarker(
  sessionId: string,
  marker: string,
  timeoutMs = 30_000,
): Promise<PaintedMarkerProof> {
  const deadline = performance.now() + timeoutMs;
  while (performance.now() <= deadline) {
    const first = findPaintedMarker(sessionId, marker);
    if (first) {
      await frames(2);
      const confirmed = findPaintedMarker(sessionId, marker);
      if (
        confirmed
        && confirmed.rowNode === first.rowNode
        && stableRect(confirmed.markerRect, first.markerRect)
        && stableRect(confirmed.terminalRect, first.terminalRect)
        && stableRect(confirmed.visualViewportRect, first.visualViewportRect)
      ) {
        const monotonicMs = performance.now();
        const proof: PaintedMarkerProof = {
          proof_kind: "marker",
          sessionId,
          marker,
          monotonicMs,
          epochMs: performance.timeOrigin + monotonicMs,
          rowText: confirmed.rowText,
          markerRect: confirmed.markerRect,
          terminalRect: confirmed.terminalRect,
          visualViewportRect: confirmed.visualViewportRect,
          frames: 2,
        };
        markPhase("marker_presented", {
          sessionId,
          marker: marker.slice(0, 160),
          markerWidth: confirmed.markerRect.width,
          markerHeight: confirmed.markerRect.height,
        });
        recordTerminalGeometryProof(sessionId, proof);
        return proof;
      }
    }
    await nextFrame();
  }
  // A bare timeout names no layer. Report which watermark stalled (wire →
  // canonical → DOM), why the renderer refused to reconcile, and whether the
  // marker text reached the DOM at all but failed the visibility proof.
  const stream = terminalBrowserStreamSnapshot(sessionId);
  const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
  throw new Error(`marker was not visibly painted within ${timeoutMs}ms: ${sessionId} ${JSON.stringify(marker)} ${JSON.stringify({
    in_dom: (slot?.textContent ?? "").includes(marker),
    rows: slot?.querySelectorAll(".cell-row").length ?? 0,
    wire: stream.wire_received,
    canonical: stream.handler_canonical,
    dom: stream.dom_reconciled,
    blocked: stream.reconcile_block_reason,
    slot: stream.slot,
    visibility: stream.visibility,
  })}`);
}

function evictOldestTiming(): void {
  if (pendingTimings.size < TIMING_CAPACITY) return;
  const oldest = pendingTimings.values().next().value as PendingTiming | undefined;
  if (!oldest) return;
  oldest.removeListener?.();
  pendingTimings.delete(oldest.id);
}

/** Start a user-felt timing endpoint. Trusted-key starts at the real isTrusted keydown. */
export function beginTerminalTiming(
  kind: TerminalTimingKind,
  sessionId?: string,
): string {
  evictOldestTiming();
  const id = crypto.randomUUID();
  const now = performance.now();
  const pending: PendingTiming = {
    id,
    kind,
    sessionId,
    startedMonotonicMs: kind === "trusted_key" ? null : now,
    startedEpochMs: kind === "trusted_key" ? null : performance.timeOrigin + now,
    trustedKey: false,
  };
  if (kind === "trusted_key") {
    if (!sessionId) throw new Error("trusted_key timing requires a session id");
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(sessionId)}"]`);
    if (!slot) throw new Error(`terminal slot missing for trusted_key timing: ${sessionId}`);
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.isTrusted || !(event.target instanceof Node) || !slot.contains(event.target)) return;
      const started = performance.now();
      pending.startedMonotonicMs = started;
      pending.startedEpochMs = performance.timeOrigin + started;
      pending.trustedKey = true;
      pending.removeListener?.();
      pending.removeListener = undefined;
    };
    document.addEventListener("keydown", onKeyDown, true);
    pending.removeListener = () => document.removeEventListener("keydown", onKeyDown, true);
  }
  pendingTimings.set(id, pending);
  return id;
}

/** Finish a timing endpoint only after the shared geometric paint proof passes. */
export async function finishTerminalTiming(
  timingId: string,
  sessionId: string,
  marker: string,
  timeoutMs = 30_000,
): Promise<TerminalTimingResult> {
  const pending = pendingTimings.get(timingId);
  if (!pending) throw new Error(`unknown or expired terminal timing: ${timingId}`);
  try {
    const proof = await waitForPaintedMarker(sessionId, marker, timeoutMs);
    if (pending.sessionId && pending.sessionId !== sessionId) {
      throw new Error(`terminal timing session changed: ${pending.sessionId} -> ${sessionId}`);
    }
    if (pending.startedMonotonicMs === null || pending.startedEpochMs === null) {
      throw new Error(`terminal timing ${timingId} never observed a trusted keydown`);
    }
    return {
      ...proof,
      timingId,
      kind: pending.kind,
      startedMonotonicMs: pending.startedMonotonicMs,
      startedEpochMs: pending.startedEpochMs,
      durationMs: proof.monotonicMs - pending.startedMonotonicMs,
      trustedKey: pending.trustedKey,
    };
  } finally {
    pending.removeListener?.();
    pendingTimings.delete(timingId);
  }
}

/** `workerFp` pins the machine the flow spawns on. Without it the flow picks the
 *  most recently seen worker, which in a multi-worker harness can be a PTY
 *  FIXTURE worker whose "shell" speaks a test protocol and never runs `printf`
 *  — the flow then waits out its paint deadline on a session that can never echo. */
export async function runFlow(
  api: SmokeApi,
  options: { workerFp?: string } = {},
): Promise<{ steps: Step[]; summary: string }> {
  const steps: Step[] = [];
  const record = (name: string, pass: boolean, detail: unknown) => steps.push({ name, pass, detail });
  let sessionId: string | null = null;

  try {
    const workers = Object.entries(api.state().workers) as Array<[string, { last_seen_ms?: number }]>;
    workers.sort(([, left], [, right]) => (right.last_seen_ms ?? 0) - (left.last_seen_ms ?? 0));
    const workerFp = options.workerFp ?? workers[0]?.[0];
    record("worker_available", !!workerFp && !!api.state().workers[workerFp], { workerFp });
    if (!workerFp) return { steps, summary: "0/1 passed" };

    const shell = await api.spawnShell(workerFp, "/tmp");
    sessionId = shell.session_id;
    history.pushState({}, "", `/s/${shell.session_id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    const shellSlot = await waitFor(() => api.renderProbe(shell.session_id).found, 300);
    record("shell_painted", shellSlot, { sessionId: shell.session_id });
    await waitFor(() => api.renderProbe(shell.session_id).nonEmptyRows > 0, 300);

    const workspace = await api.createWorkspace(workerFp, "/", shell.session_id);
    record("workspace_created", !!workspace.id, workspace);

    const marker = `ROOST_SMOKE_${crypto.randomUUID().slice(0, 8)}`;
    await api.input(shell.session_id, `printf '%s\\n' ${marker}\n`);
    const painted = await api.waitForPaintedMarker(shell.session_id, marker);
    record("shell_round_trip", true, painted);
  } catch (error) {
    // The browser half of the failure is already in the message. Attach the
    // coordinator and worker halves too, so a stall is attributed to a layer
    // instead of re-run until it reproduces under a debugger.
    const layers = sessionId
      ? await api.terminalStreamProbe(sessionId).catch((probeError: unknown) => ({
        probe_failed: String(probeError),
      }))
      : null;
    record("flow_exception", false, { error: String(error), layers });
  } finally {
    const cleanup = await api.cleanupCreated();
    record("cleanup", cleanup.errors.length === 0, cleanup);
  }

  const passed = steps.filter((step) => step.pass).length;
  return { steps, summary: `${passed}/${steps.length} passed` };
}

export async function runRenderStress(
  api: SmokeApi,
  options: { sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number },
): Promise<{ verdict: "PASS" | "FAIL"; iterations: number; failCount: number; fails: unknown[] }> {
  const deck = document.querySelector('[data-testid="terminal-deck"]') as HTMLElement | null;
  if (!deck) return { verdict: "FAIL", iterations: 0, failCount: 1, fails: ["terminal deck missing"] };

  const original = deck.getAttribute("style");
  const perturbations = [[-200, 0], [200, 0], [0, -180], [0, 180], [-200, -180], [200, 180]] as const;
  const baseline = api.markerScan(options.sessionId, options.prefix);
  const fails: unknown[] = [];
  try {
    for (let iteration = 0; iteration < options.iterations; iteration++) {
      const [x, y] = perturbations[iteration % perturbations.length]!;
      const rect = deck.getBoundingClientRect();
      deck.style.width = `${Math.max(220, Math.round(rect.width + x))}px`;
      deck.style.height = `${Math.max(180, Math.round(rect.height + y))}px`;
      const beforeFrames = api.cellFrameCount(options.sessionId);
      const mode = api.renderProbe(options.sessionId).mode;
      if (mode === "cell") {
        await waitFor(() => api.cellFrameCount(options.sessionId) > beforeFrames, 120);
      } else {
        await frames(6);
      }
      await frames(2);
      const scan = api.markerScan(options.sessionId, options.prefix);
      // markerScan reads the PAINTED viewport: a shorter deck shows fewer rows, so a rising scan.min is normal — a moving scan.max is not, it means live history was lost rather than scrolled out.
      if (scan.duplicated.length || scan.outOfOrder || (options.screen === "main" && scan.max !== baseline.max)) {
        fails.push({ iteration, screen: options.screen, scan, baseline });
      }
    }
  } finally {
    if (original === null) deck.removeAttribute("style");
    else deck.setAttribute("style", original);
  }
  return { verdict: fails.length === 0 ? "PASS" : "FAIL", iterations: options.iterations, failCount: fails.length, fails };
}

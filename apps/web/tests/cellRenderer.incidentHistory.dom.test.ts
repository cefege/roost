// Painted-history incident capture at the renderer's DOM boundary.
// The armed recorder must freeze a DOM that violates a painted-history
// invariant BEFORE the renderer's own repair replaces those nodes, and must
// stay silent for every legitimate divergence between model and paint.
// Scroll-space and gap-page behaviour is owned by cellRenderer.history.dom.test.ts.

import { beforeEach, describe, expect, test } from "bun:test";
import { CellGridRenderer, MAX_HELD_SCROLLBACK_ROWS } from "../src/lib/cellRenderer.ts";
import type { CellGridFrame, CellRow } from "@roost/shared/cell";
import type { TerminalBrowserPaintedState } from "@roost/shared/terminal-capture";
import { createIncidentObserver } from "../src/lib/terminalIncidentCaptureObserver.ts";
import {
  _resetTerminalIncidentRecorders,
  armTerminalRecorder,
  ensureTerminalRecorder,
  type TerminalIncidentRecorder,
} from "../src/lib/terminalIncidentCaptureState.ts";
import {
  PAD_TOP,
  ROW_PX,
  makeContainer,
  row,
  fullFrame,
  altFullFrame,
  altDeltaFrame,
  deltaFrame,
  injectDuplicateHistoryNode,
  seedHeldHistory,
  sbEl,
  sbRows,
} from "./helpers/cellRendererFakeDom.ts";

describe("CellGridRenderer DOM — terminal incident capture", () => {
  const SESSION = "11111111-1111-4111-8111-111111111111";
  const nRows = (n: number, from = 0) =>
    Array.from({ length: n }, (_, i) => row(from + i, `r${from + i}`));
  const appendDelta = (append: CellRow[], total: number, seq: number): CellGridFrame =>
    ({ ...deltaFrame(80, 1, [row(0, `v${seq}`)], append, seq), scrollbackTotal: total });

  interface ArmedRecording {
    recorder: TerminalIncidentRecorder;
    triggers: number;
  }

  function armRecording(renderer: CellGridRenderer): ArmedRecording {
    const recorder = ensureTerminalRecorder(SESSION, "rec-0000");
    armTerminalRecorder(recorder, Date.now() + 600_000);
    const armed: ArmedRecording = { recorder, triggers: 0 };
    renderer.incidentObserver = createIncidentObserver(recorder, renderer, () => {
      armed.triggers++;
    });
    return armed;
  }

  const domIndices = (state: TerminalBrowserPaintedState | null): Array<number | null> =>
    (state?.dom_history ?? []).map((entry) => entry.index);

  beforeEach(() => {
    _resetTerminalIncidentRecorders();
  });

  test("a duplicated absolute history index is frozen before the repair removes it", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(10, 0), 10);
    const armed = armRecording(r);
    expect(r.apply(appendDelta([row(10, "h10")], 11, 3))).toBe(true);
    expect(armed.recorder.committed).not.toBeNull();

    injectDuplicateHistoryNode(c, 9);
    expect(sbRows(sbEl(c)).filter((el) => el.dataset.rowIndex === "9")).toHaveLength(2);

    // One ordinary output update: the renderer inserts the next history row.
    expect(r.apply(appendDelta([row(11, "h11")], 12, 4))).toBe(true);

    expect(armed.triggers).toBe(1);
    expect(armed.recorder.trigger?.detail).toBe("history_duplicate_index");
    expect(armed.recorder.trigger?.reason).toBe("history_identity");
    const frozen = armed.recorder.triggerState;
    expect(domIndices(frozen).filter((index) => index === 9)).toHaveLength(2);
    // Frozen at the mutation boundary, not after the repair healed the DOM.
    expect(frozen?.phase).toBe("pre_history_insert");
    expect(armed.recorder.preRepairState).not.toBeNull();

    // A later destructive repair heals the DOM; the frozen evidence keeps both.
    expect(r.applyFullFrame({
      ...fullFrame(100, [row(0, "after-repair")], 12),
      gridEpoch: "test-grid:1",
      seq: 5,
    })).toBe(true);
    expect(sbRows(sbEl(c)).filter((el) => el.dataset.rowIndex === "9")).toHaveLength(0);
    expect(domIndices(armed.recorder.triggerState).filter((index) => index === 9)).toHaveLength(2);
  });

  test("identical text at distinct absolute indices stays silent", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const repeated = [row(0, "FOOTER 12s"), row(1, "FOOTER 12s"), row(2, "FOOTER 12s")];
    seedHeldHistory(r, 80, [row(0, "v")], repeated, 3);
    const armed = armRecording(r);

    expect(r.apply(appendDelta([row(3, "FOOTER 12s")], 4, 3))).toBe(true);
    expect(r.apply(appendDelta([row(4, "FOOTER 12s")], 5, 4))).toBe(true);

    expect(armed.triggers).toBe(0);
    expect(armed.recorder.trigger).toBeNull();
  });

  test("a reader-held DOM behind newer canonical frames stays silent", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(10, 0), 10);
    const armed = armRecording(r);
    expect(r.apply(appendDelta([row(10, "h10")], 11, 3))).toBe(true);

    r.setSelectionHold(true);
    expect(r.apply(appendDelta([row(11, "h11")], 12, 4))).toBe(true);
    expect(r.apply({ ...fullFrame(80, [row(0, "newer")], 12), seq: 5 })).toBe(true);
    c.scrollTop = PAD_TOP + 2 * ROW_PX;
    r.handleScroll();

    expect(armed.triggers).toBe(0);
  });

  test("resize, alt-screen and eviction transitions stay silent", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    seedHeldHistory(r, 80, [row(0, "v")], nRows(10, 0), 10);
    const armed = armRecording(r);
    expect(r.apply(appendDelta([row(10, "h10")], 11, 3))).toBe(true);

    // Resize: a new geometry repaints the whole grid.
    expect(r.applyFullFrame({ ...fullFrame(100, [row(0, "wide")], 11), seq: 4 })).toBe(true);
    // Alt screen: a new grid epoch.
    expect(r.applyFullFrame(altFullFrame(100, [row(0, "alt")], []))).toBe(true);
    expect(r.apply(altDeltaFrame(100, 1, [row(0, "alt-2")], 2))).toBe(true);

    expect(armed.triggers).toBe(0);
  });

  test("history eviction and a same-epoch full repair stay silent", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    const held = MAX_HELD_SCROLLBACK_ROWS - 10;
    seedHeldHistory(r, 80, [row(0, "v")], nRows(held, 0), held);
    const armed = armRecording(r);

    // Push past the retained-row cap so the evictor drops the leading block.
    expect(r.apply(appendDelta(nRows(100, held), held + 100, 3))).toBe(true);
    expect(r.paintedScrollbackRowCount()).toBeLessThanOrEqual(MAX_HELD_SCROLLBACK_ROWS);
    // A compatible same-epoch full repair retains that painted history.
    expect(r.applyFullFrame({
      ...fullFrame(80, [row(0, "repaired")], held + 100),
      seq: 4,
    })).toBe(true);

    expect(armed.triggers).toBe(0);
  });

  test("an unarmed terminal never reads the DOM through the projection", () => {
    const c = makeContainer();
    const r = new CellGridRenderer(c as unknown as HTMLElement);
    let projections = 0;
    const readProjection = r.rendererProjection.bind(r);
    r.rendererProjection = () => {
      projections++;
      return readProjection();
    };

    seedHeldHistory(r, 80, [row(0, "v")], nRows(10, 0), 10);
    injectDuplicateHistoryNode(c, 9);
    expect(r.apply(appendDelta([row(10, "h10")], 11, 3))).toBe(true);
    expect(r.applyFullFrame({ ...fullFrame(100, [row(0, "repaint")], 11), seq: 4 })).toBe(true);

    expect(r.incidentObserver).toBeNull();
    expect(projections).toBe(0);
  });
});

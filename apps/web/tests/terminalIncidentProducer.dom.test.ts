// What an armed browser recorder actually EMITS when a painted invariant
// fires: one Tier-1 line per (lease, stream, epoch, reason) identity with the
// repeats carried as counters, and a frozen payload that passes the same
// envelope check the coordinator bridge runs before forwarding evidence.
// The Tier-1 channel is captured here instead of the shared per-kind cooldown,
// because that cooldown is exactly what a persistent violation defeats.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { CellGridFrame, CellRow } from "@roost/protocol/cell";
import {
  checkTerminalCaptureEnvelope,
  TERMINAL_INCIDENT_SCHEMA,
  type TerminalCaptureCommand,
} from "@roost/protocol/terminal-capture";
import {
  makeContainer,
  row,
  fullFrame,
  deltaFrame,
  injectDuplicateHistoryNode,
  seedHeldHistory,
} from "./helpers/cellRendererFakeDom.ts";

interface SignalLine {
  readonly evt: string;
  readonly kind?: unknown;
  readonly grid_epoch?: unknown;
  readonly occurrences?: unknown;
  readonly captured?: unknown;
}

const signals: SignalLine[] = [];
const realDiag = await import("@roost/observability/diag");

// The real per-kind cooldown would coalesce repeats on its own, which is the
// behaviour under test; this sink records every call the recorder makes.
mock.module("@roost/observability/diag", () => ({
  ...realDiag,
  signal(kind: string, kv: Record<string, unknown>) {
    signals.push({ evt: kind, ...kv } as SignalLine);
  },
}));

// Deferred so the diagnostic mock is installed before the renderer and the
// recorder evaluate their static imports.
const { CellGridRenderer } = await import("../src/lib/cellRenderer.ts");
const { createIncidentObserver } = await import(
  "../src/lib/terminalIncidentCaptureObserver.ts"
);
const { freezeRecorderEvidence } = await import(
  "../src/lib/terminalIncidentCaptureEvidence.ts"
);
const {
  _resetTerminalIncidentRecorders,
  armTerminalRecorder,
  ensureTerminalRecorder,
} = await import("../src/lib/terminalIncidentCaptureState.ts");

const SESSION = "11111111-1111-4111-8111-111111111111";
const RECORDING = "33333333-3333-4333-8333-333333333333";

describe("terminal incident capture — browser producer", () => {
  const nRows = (count: number, from = 0): CellRow[] =>
    Array.from({ length: count }, (_, at) => row(from + at, `r${from + at}`));
  const appendDelta = (
    append: CellRow[],
    total: number,
    seq: number,
    gridEpoch = "test-grid:0",
  ): CellGridFrame => ({
    ...deltaFrame(80, 1, [row(0, `v${seq}`)], append, seq),
    scrollbackTotal: total,
    gridEpoch,
  });

  function armedPane() {
    const container = makeContainer();
    const renderer = new CellGridRenderer(container as unknown as HTMLElement);
    seedHeldHistory(renderer, 80, [row(0, "v")], nRows(10), 10);
    const recorder = ensureTerminalRecorder(SESSION, RECORDING);
    armTerminalRecorder(recorder, Date.now() + 600_000);
    let triggers = 0;
    renderer.incidentObserver = createIncidentObserver(recorder, renderer, () => {
      triggers++;
    });
    expect(renderer.apply(appendDelta([row(10, "h10")], 11, 3))).toBe(true);
    return {
      container,
      renderer,
      recorder,
      triggers: () => triggers,
    };
  }

  beforeEach(() => {
    _resetTerminalIncidentRecorders();
    signals.length = 0;
  });

  test("a persistent conflict signals once per identity and counts the rest", () => {
    const pane = armedPane();
    injectDuplicateHistoryNode(pane.container, 9);

    // Five ordinary output updates while the duplicate stays painted. Each one
    // observes the violation at the history-insert boundary and again after
    // reconciliation, so the recorder sees ten occurrences of one identity.
    for (let at = 0; at < 5; at++) {
      expect(pane.renderer.apply(appendDelta([row(11 + at, `h${11 + at}`)], 12 + at, 4 + at)))
        .toBe(true);
    }

    expect(signals.map((line) => line.evt)).toEqual(["terminal.history_conflict"]);
    expect(signals[0]?.kind).toBe("history_duplicate_index");
    expect(signals[0]?.captured).toBe(true);
    expect(pane.triggers()).toBe(1);
    expect(pane.recorder.conflicts).toEqual({
      occurrences: 10,
      identities: 1,
      captured: 1,
      dropped_identities: 0,
    });
  });

  test("a conflict at a new grid epoch is a new identity and reports once", () => {
    const pane = armedPane();
    injectDuplicateHistoryNode(pane.container, 9);
    expect(pane.renderer.apply(appendDelta([row(11, "h11")], 12, 4))).toBe(true);
    expect(signals).toHaveLength(1);

    // A new epoch rebuilds the paint, which heals the injected duplicate.
    expect(pane.renderer.applyFullFrame({
      ...fullFrame(80, [row(0, "e2")], 40),
      gridEpoch: "test-grid:9",
      seq: 20,
    })).toBe(true);
    expect(pane.renderer.apply(appendDelta(nRows(5, 40), 45, 21, "test-grid:9"))).toBe(true);
    injectDuplicateHistoryNode(pane.container, 44);
    expect(pane.renderer.apply(appendDelta([row(45, "h45")], 46, 22, "test-grid:9"))).toBe(true);

    expect(signals).toHaveLength(2);
    expect(signals[1]?.grid_epoch).toBe("test-grid:9");
    expect(signals[1]?.occurrences).toBe(1);
    // The session-wide automatic cooldown still owns the capture: a new
    // identity buys a report, not a second bundle.
    expect(signals[1]?.captured).toBe(false);
    expect(pane.recorder.conflicts.identities).toBe(2);
    expect(pane.recorder.conflicts.captured).toBe(1);
    expect(pane.triggers()).toBe(1);
  });

  test("frozen evidence passes the bridge envelope check with its own section", () => {
    const pane = armedPane();
    injectDuplicateHistoryNode(pane.container, 9);
    expect(pane.renderer.apply(appendDelta([row(11, "h11")], 12, 4))).toBe(true);
    expect(pane.recorder.trigger).not.toBeNull();

    const frozen = freezeRecorderEvidence(pane.recorder, "history_identity");
    const command: TerminalCaptureCommand = {
      action: "capture",
      session_id: SESSION,
      recording_id: RECORDING,
      capture_id: frozen.captureId,
      reason: "history_identity",
      browser_evidence_json: frozen.wireJson,
    };

    const checked = checkTerminalCaptureEnvelope(frozen.wireJson, {
      layer: "browser",
      command,
    });
    expect(checked.ok).toBe(true);
    if (!checked.ok) return;
    expect(checked.envelope.schema).toBe(TERMINAL_INCIDENT_SCHEMA);
    // The section is NESTED under `browser`, never flattened onto the
    // envelope: a flattened payload still validates as an envelope and drops
    // the whole browser layer from the bundle.
    expect(typeof checked.section.captured_at_ms).toBe("number");
    expect(checked.section.layer).toBe("browser");
    expect(checked.trigger?.detail).toBe("history_duplicate_index");
    expect(checked.trigger?.origin).toBe("browser");
    expect(checked.trigger?.occurrence_count).toBe(1);
  });
});

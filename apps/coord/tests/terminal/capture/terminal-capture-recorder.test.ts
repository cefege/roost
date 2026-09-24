// Coordinator capture records: the TerminalScreenHub hooks, the retention
// bounds, and the freeze that must never hand a replay an orphan delta.
// Hub admission runs through the real harness so the hook wiring is proven,
// not simulated; the bound cases call the recorder directly because they need
// more frames than a readable hub script.

import { afterEach, describe, expect, test } from "bun:test";

import {
  TERMINAL_CAPTURE_LIMITS,
  TERMINAL_INCIDENT_SCHEMA,
  checkTerminalCaptureEnvelope,
  utf8ByteLength,
  type TerminalCaptureCommand,
  type TerminalCaptureCoordinatorPayload,
  type TerminalCoordinatorSection,
} from "@roost/protocol/terminal-capture";
import {
  armCoordinatorRecorder,
  coordinatorRecorderArmed,
  disarmCoordinatorRecorder,
  freezeCoordinatorEvidence,
  recordCoordinatorFrame,
  _coordinatorRecorderRecords,
  _coordinatorRecorderStats,
  _resetCoordinatorRecorder,
} from "../../../src/terminal/capture/terminal-capture-recorder.ts";
import {
  EPOCH,
  SESSION,
  STREAM,
  TestSink,
  deltaFrame,
  fullFrame,
  makeHarness,
  watch,
} from "../screen/terminal-screen-hub-harness.ts";
import { withoutLayerSection } from "./terminal-capture-evidence.ts";
import {
  CAPTURE_1,
  RECORDING_A,
  RECORDING_B,
  canonicalCaptureFrame,
  captureCommand,
} from "./terminal-capture-harness.ts";

const NO_WATCHERS = new Map<string, ReadonlySet<string>>();
const ONE_WATCHER = new Map<string, ReadonlySet<string>>([[SESSION, new Set(["socket-a"])]]);

const CAPTURE_COMMAND = captureCommand({ session_id: SESSION });

/** The frozen payload, its envelope and its nested section. The wire bound is
 *  UTF-8 bytes of the WHOLE payload, one nesting level included. */
function frozenEvidence(): {
  json: string;
  payload: TerminalCaptureCoordinatorPayload;
  section: TerminalCoordinatorSection;
  bytes: number;
} {
  const evidence = freezeCoordinatorEvidence(SESSION, CAPTURE_1, RECORDING_A);
  expect(evidence.available).toBe(true);
  const bytes = utf8ByteLength(evidence.json);
  expect(bytes).toBeLessThanOrEqual(TERMINAL_CAPTURE_LIMITS.coordinatorEvidenceBytes);
  expect(evidence.bytes).toBe(bytes);
  const payload = JSON.parse(evidence.json) as TerminalCaptureCoordinatorPayload;
  return { json: evidence.json, payload, section: payload.coordinator, bytes };
}

function frozenSection(): TerminalCoordinatorSection {
  return frozenEvidence().section;
}

afterEach(() => {
  _resetCoordinatorRecorder();
});

describe("coordinator capture records", () => {
  test("an unarmed session retains nothing and reports its layer unavailable", () => {
    const { hub } = makeHarness();
    watch(hub, new TestSink());
    hub.expectStream(SESSION, STREAM, 8, 2);
    hub.publishFrame(SESSION, fullFrame({ texts: ["old-a", "old-b"] }));
    hub.publishFrame(SESSION, deltaFrame({ text: "new-b" }));

    expect(coordinatorRecorderArmed(SESSION)).toBe(false);
    expect(_coordinatorRecorderStats(SESSION)).toBeNull();
    expect(_coordinatorRecorderRecords(SESSION)).toEqual([]);
    const evidence = freezeCoordinatorEvidence(SESSION, CAPTURE_1, RECORDING_A);
    expect(evidence).toMatchObject({ available: false, json: "", records: 0 });
  });

  test("records the admitted full and the folded delta with the hub's canonical", () => {
    const { hub } = makeHarness();
    watch(hub, new TestSink());
    hub.expectStream(SESSION, STREAM, 8, 2);
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());

    hub.publishFrame(SESSION, fullFrame({ texts: ["old-a", "old-b"] }));
    hub.publishFrame(SESSION, deltaFrame({ text: "new-b" }));

    const records = _coordinatorRecorderRecords(SESSION);
    expect(records).toHaveLength(2);
    expect(records[0]).toMatchObject({
      admitted_full: true,
      accepted: true,
      snapshot_state: "installed",
      send_state: "queued",
      gap: null,
      repair: "none",
      stream: { stream_id: STREAM, grid_epoch: EPOCH, seq: "1", base_seq: null, cols: 8, rows: 2 },
    });
    expect(records[1]).toMatchObject({
      admitted_full: false,
      accepted: true,
      gap: null,
      repair: "none",
      stream: { seq: "2", base_seq: "1" },
    });
    // The retained canonical is the hub's own post-admission viewport, so the
    // folded row is visible without re-deriving the grid.
    const folded = records[1]!.canonical;
    expect(folded?.full).toBe(true);
    expect(folded?.viewportRows.map((row) => row.spans.map((entry) => entry.text).join("")))
      .toEqual(["old-a", "new-b"]);
    expect(records[0]!.canonical?.viewportRows[1]?.spans[0]?.text).toBe("old-b");
  });

  test("reports a repaired baseline as a sequence gap, and no watcher as not sent", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    recordCoordinatorFrame(SESSION, canonicalCaptureFrame({ seq: 4 }), { full: true, seq: 4n, baseSeq: 0n }, NO_WATCHERS);
    // A fresh full that skips sequences is the coordinator asking the worker
    // for a new baseline after its own replica failed closed.
    recordCoordinatorFrame(SESSION, canonicalCaptureFrame({ seq: 9 }), { full: true, seq: 9n, baseSeq: 0n }, ONE_WATCHER);

    const records = _coordinatorRecorderRecords(SESSION);
    expect(records[0]).toMatchObject({ send_state: "not_sent", gap: null, repair: "none" });
    expect(records[1]).toMatchObject({
      send_state: "queued",
      gap: { from: "4", to: "9" },
      repair: "requested_full",
    });
    // A new epoch restarts the numbering by design and is not a gap.
    recordCoordinatorFrame(
      SESSION,
      canonicalCaptureFrame({ seq: 1, epoch: "grid-epoch-b" }),
      { full: true, seq: 1n, baseSeq: 0n },
      ONE_WATCHER,
    );
    expect(_coordinatorRecorderRecords(SESSION)[2]).toMatchObject({ gap: null, repair: "none" });
  });

  test("a renewal keeps evidence; a different recording starts empty", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    recordCoordinatorFrame(SESSION, canonicalCaptureFrame({ seq: 1 }), { full: true, seq: 1n, baseSeq: 0n }, NO_WATCHERS);
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    expect(_coordinatorRecorderStats(SESSION)).toMatchObject({ recordingId: RECORDING_A, records: 1 });

    armCoordinatorRecorder(SESSION, RECORDING_B, Date.now());
    expect(_coordinatorRecorderStats(SESSION)).toMatchObject({ recordingId: RECORDING_B, records: 0 });
    // Evidence belongs to one recording: another page's capture cannot read it.
    expect(freezeCoordinatorEvidence(SESSION, CAPTURE_1, RECORDING_A).available).toBe(false);
    expect(disarmCoordinatorRecorder(SESSION)).toMatchObject({ records: 0 });
    expect(coordinatorRecorderArmed(SESSION)).toBe(false);
  });

  test("entry eviction keeps the retained head a complete checkpoint", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    const total = TERMINAL_CAPTURE_LIMITS.layerEntries + 8;
    for (let seq = 1; seq <= total; seq++) {
      recordCoordinatorFrame(
        SESSION,
        canonicalCaptureFrame({ seq }),
        { full: seq === 1, seq: BigInt(seq), baseSeq: BigInt(seq - 1) },
        NO_WATCHERS,
      );
    }
    const records = _coordinatorRecorderRecords(SESSION);
    expect(records).toHaveLength(TERMINAL_CAPTURE_LIMITS.layerEntries);
    expect(records[0]!.canonical).not.toBeNull();
    expect(records[records.length - 1]!.stream.seq).toBe(String(total));
    expect(_coordinatorRecorderStats(SESSION)).toMatchObject({ dropped: 8 });
  });

  test("a frame over the wire budget is retained as metadata and never left at the head", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    // Wider than the whole coordinator evidence budget: keeping its rows could
    // never be shipped, so the record stays and its canonical does not.
    recordCoordinatorFrame(
      SESSION,
      canonicalCaptureFrame({ seq: 1, rows: 64, spansPerRow: 64, text: "x".repeat(120) }),
      { full: true, seq: 1n, baseSeq: 0n },
      NO_WATCHERS,
    );
    expect(_coordinatorRecorderRecords(SESSION)[0]!.canonical).toBeNull();

    recordCoordinatorFrame(SESSION, canonicalCaptureFrame({ seq: 2 }), { full: false, seq: 2n, baseSeq: 1n }, NO_WATCHERS);
    const records = _coordinatorRecorderRecords(SESSION);
    expect(records).toHaveLength(1);
    expect(records[0]).toMatchObject({ stream: { seq: "2" }, admitted_full: false });
    expect(records[0]!.canonical).not.toBeNull();

    const section = frozenSection();
    expect(section.dropped).toMatchObject({ rows: 64, raw_bytes: 0, samples: 0 });
    expect(section.omissions).toEqual(expect.arrayContaining([
      expect.objectContaining({ reason: "frame_over_budget", dropped_count: 1 }),
    ]));
  });

  test("freezing trims oldest whole records to the budget and names the omission", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    for (let seq = 1; seq <= 32; seq++) {
      recordCoordinatorFrame(
        SESSION,
        canonicalCaptureFrame({ seq, rows: 24, spansPerRow: 12, text: "y".repeat(80) }),
        { full: seq === 1, seq: BigInt(seq), baseSeq: BigInt(seq - 1) },
        ONE_WATCHER,
      );
    }
    const section = frozenSection();
    const records = section.records;
    expect(records.length).toBeGreaterThan(0);
    expect(records.length).toBeLessThan(32);
    // The newest evidence is what the incident needs, and the head still
    // carries a canonical a replay can start from.
    expect(records[records.length - 1]!.stream.seq).toBe("32");
    expect(records[0]!.canonical).not.toBeNull();
    expect(section.omissions).toEqual(expect.arrayContaining([
      expect.objectContaining({
        reason: "evidence_trimmed",
        dropped_count: 32 - records.length,
      }),
    ]));
  });

  test("the payload nests its section under the layer and keeps identity on the envelope", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    recordCoordinatorFrame(
      SESSION,
      canonicalCaptureFrame({ seq: 7 }),
      { full: true, seq: 7n, baseSeq: 0n },
      ONE_WATCHER,
    );
    const { payload, section } = frozenEvidence();

    // Envelope: capture identity, and only that.
    expect(Object.keys(payload).sort())
      .toEqual(["capture_id", "coordinator", "layer", "recording_id", "schema", "session_id"]);
    expect(payload).toMatchObject({
      schema: TERMINAL_INCIDENT_SCHEMA,
      layer: "coordinator",
      capture_id: CAPTURE_1,
      recording_id: RECORDING_A,
      session_id: SESSION,
    });
    // Section: the layer's own evidence, NOT the envelope's identity. A
    // flattened payload passes an identity check and then fails as a section,
    // which drops this whole layer from the bundle with nothing to read.
    expect(section).toMatchObject({
      layer: "coordinator",
      valid: true,
      geometry: { cols: 80, rows: 2 },
      snapshot: { stream_id: STREAM, seq: "7" },
    });
    expect(section.records).toHaveLength(1);
    expect(typeof section.captured_at_ms).toBe("number");
    expect(section.process).toMatchObject({
      layer: "coordinator",
      wasm_identity: null,
      worker_fp: null,
      viewer_id: null,
      user_agent: null,
    });
    expect(typeof section.process.process_id).toBe("string");
    expect(section.omissions).toEqual([]);
    const envelopeKeys = payload as unknown as Record<string, unknown>;
    expect(envelopeKeys.records).toBeUndefined();
    expect(envelopeKeys.captured_at_ms).toBeUndefined();
  });

  test("the frozen wire JSON itself is admitted through its nested section", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    recordCoordinatorFrame(
      SESSION,
      canonicalCaptureFrame({ seq: 4 }),
      { full: true, seq: 4n, baseSeq: 0n },
      ONE_WATCHER,
    );
    // The exact string the bridge forwards, never a re-serialized fixture: a
    // hand-built payload proves the checker, not the producer.
    const { json, payload } = frozenEvidence();

    const admitted = checkTerminalCaptureEnvelope(json, {
      layer: "coordinator",
      command: CAPTURE_COMMAND,
    });
    expect(admitted.ok).toBe(true);
    if (!admitted.ok) throw new Error("the frozen coordinator payload must be admitted");
    // Reached through the `coordinator` member. A flattened payload satisfies
    // the identity check and then carries no section, which drops this whole
    // layer from the bundle with one omission line to show for it.
    expect(admitted.section.records).toHaveLength(1);
    expect(typeof admitted.section.captured_at_ms).toBe("number");
    // The coordinator never authors a trigger; the triggering layer ships it.
    expect(admitted.trigger).toBeNull();
    expect(admitted.envelope).toMatchObject({
      layer: "coordinator",
      capture_id: CAPTURE_1,
      session_id: SESSION,
    });

    // The production defect: the capture identity still matches, so only the
    // section check can catch it.
    expect(checkTerminalCaptureEnvelope(
      JSON.stringify(withoutLayerSection(payload)),
      { layer: "coordinator", command: CAPTURE_COMMAND },
    )).toEqual({ ok: false, code: "evidence_malformed", field: "coordinator" });
  });

  test("a maximally trimmed payload still fits the coordinator evidence budget", () => {
    armCoordinatorRecorder(SESSION, RECORDING_A, Date.now());
    // Every omission kind at once — eviction, over-budget rows and the freeze
    // trim — so the header is at its largest while records fill the rest.
    recordCoordinatorFrame(
      SESSION,
      canonicalCaptureFrame({ seq: 1, rows: 64, spansPerRow: 64, text: "z".repeat(120) }),
      { full: true, seq: 1n, baseSeq: 0n },
      ONE_WATCHER,
    );
    const total = TERMINAL_CAPTURE_LIMITS.layerEntries + 4;
    for (let seq = 2; seq <= total; seq++) {
      recordCoordinatorFrame(
        SESSION,
        canonicalCaptureFrame({ seq, rows: 24, spansPerRow: 16, text: "w".repeat(96) }),
        { full: false, seq: BigInt(seq), baseSeq: BigInt(seq - 1) },
        ONE_WATCHER,
      );
    }
    const { section, bytes } = frozenEvidence();
    expect(bytes).toBeLessThanOrEqual(TERMINAL_CAPTURE_LIMITS.coordinatorEvidenceBytes);
    // The reserve must leave room for the envelope AND the header it names, so
    // a trimmed capture still ships evidence instead of reporting unavailable.
    expect(section.records.length).toBeGreaterThan(0);
    expect(section.records[0]!.canonical).not.toBeNull();
    expect(section.omissions.map((omission) => omission.reason).sort())
      .toEqual(["evidence_trimmed", "frame_over_budget", "segment_evicted"]);
    expect(section.dropped.records).toBeGreaterThan(0);
  });
});

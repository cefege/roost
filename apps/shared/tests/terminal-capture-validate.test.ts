// Incident-bundle validation contract. This validator is the gate that stands
// between a corrupt or truncated capture and an attribution claim, so the
// assertions here are about what must be REFUSED: a bundle that would let a
// replay fold onto a state nobody shipped, or read a byte offset that rounded.
//
// Rejections must also stay content-free: the validator reports a code and a
// field path, never the value, because every value it inspects is terminal text.

import { describe, expect, test } from "bun:test";
import { TERMINAL_CAPTURE_LIMITS } from "../src/terminal-capture.ts";
import {
  isDecimalUint64,
  validateTerminalIncidentBundle,
} from "../src/terminal-capture-validate.ts";

const SESSION = "22222222-2222-4222-8222-222222222222";
const CAPTURE = "33333333-3333-4333-8333-333333333333";
const RECORDING = "44444444-4444-4444-8444-444444444444";
const STREAM = "11111111-1111-4111-8111-111111111111";

function streamIdentity(seq: string) {
  return {
    stream_id: STREAM,
    grid_epoch: "epoch:1",
    seq,
    base_seq: null,
    cols: 4,
    rows: 1,
  };
}

function fullFrame(seq: number) {
  return {
    streamId: STREAM,
    gridEpoch: "epoch:1",
    cols: 4,
    rows: 1,
    cursorRow: 0,
    cursorCol: 0,
    cursorVisible: true,
    altScreen: false,
    cursorKeysApp: false,
    bracketedPaste: false,
    mouseTracking: 0,
    mouseSgr: false,
    focusEvents: false,
    full: true,
    viewportRows: [
      { index: 0, spans: [{ text: "ok", fg: 256, bg: 256, flags: 0, columns: 2 }] },
    ],
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0,
    sbBase: 0,
    baseSeq: 0,
    seq,
  };
}

function workerSection(over: Record<string, unknown> = {}) {
  return {
    layer: "worker",
    captured_at_ms: 1_700_000_000_000,
    process: {
      layer: "worker",
      process_id: "worker-1",
      git_sha: "abc1234",
      artifact_version: "2.0.0",
      wasm_identity: "sha256:dead",
      worker_fp: "fp1",
      viewer_id: null,
      user_agent: null,
    },
    stream: streamIdentity("7"),
    geometry: { cols: 4, rows: 1 },
    dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
    omissions: [],
    segments: [{
      segment_id: "seg-1",
      stream_id: STREAM,
      grid_epoch: "epoch:1",
      core_incarnation: 0,
      opened_at_ms: 1_700_000_000_000,
      closed_at_ms: null,
      open_reason: "armed",
      geometry: { cols: 4, rows: 1 },
      open_offset: "0",
    }],
    emissions: [{
      segment_id: "seg-1",
      emitted_at_ms: 1_700_000_000_001,
      stream: streamIdentity("1"),
      full: true,
      frame: fullFrame(1),
      comparison: "equal",
      difference: null,
    }],
    core_samples: [],
    sampling: {
      sampled: 0, skipped_interval: 0, skipped_budget: 0, skipped_grid: 0,
      suppressed_until_ms: null, max_elapsed_us: 0,
    },
    resizes: [],
    raw: [{
      segment_id: "seg-1",
      at_ms: 1_700_000_000_001,
      start_offset: "0",
      end_offset: "2",
      base64: "b2s=",
    }],
    byte_capture: null,
    core_scrollback_tail: [],
    history_rows: [],
    history_ranges: [],
    scrollback_total: 0,
    scrollback_origin: "0",
    ...over,
  };
}

function bundle(over: Record<string, unknown> = {}) {
  return {
    schema: "roost.terminal-incident.v1",
    capture_id: CAPTURE,
    recording_id: RECORDING,
    session_id: SESSION,
    written_at_ms: 1_700_000_000_002,
    trigger: {
      reason: "manual",
      origin: "worker",
      at_ms: 1_700_000_000_002,
      stream_id: STREAM,
      grid_epoch: "epoch:1",
      seq: "1",
      detail: null,
      occurrence_count: 1,
    },
    coverage: {
      cell_replay: "complete",
      cell_replay_reasons: ["complete"],
      core_replay: "partial",
      core_replay_reasons: ["missing_initial_prefix"],
      core_comparison: "unavailable",
      core_comparison_reasons: ["layer_unavailable"],
    },
    browser: null,
    coordinator: null,
    worker: workerSection(),
    ...over,
  };
}

describe("validateTerminalIncidentBundle", () => {
  test("accepts a minimal worker-only bundle", () => {
    const result = validateTerminalIncidentBundle(bundle());
    expect(result.ok).toBe(true);
  });

  test("rejects a foreign or absent schema literal", () => {
    expect(validateTerminalIncidentBundle(bundle({ schema: "other.v1" }))).toMatchObject({
      ok: false, code: "evidence_malformed", field: "schema",
    });
  });

  test("rejects a non-UUID identifier", () => {
    expect(validateTerminalIncidentBundle(bundle({ capture_id: "not-a-uuid" }))).toMatchObject({
      ok: false, code: "invalid_argument", field: "capture_id",
    });
  });

  test("requires a coverage reason for every coverage axis", () => {
    const broken = bundle();
    (broken.coverage as Record<string, unknown>).core_replay_reasons = [];
    expect(validateTerminalIncidentBundle(broken)).toMatchObject({
      ok: false, field: "coverage.core_replay_reasons",
    });
  });

  test("rejects a per-segment sequence that moves backwards", () => {
    const rewound = workerSection({
      emissions: [
        {
          segment_id: "seg-1", emitted_at_ms: 1_700_000_000_001,
          stream: streamIdentity("5"), full: true, frame: fullFrame(5),
          comparison: "equal", difference: null,
        },
        {
          segment_id: "seg-1", emitted_at_ms: 1_700_000_000_002,
          stream: streamIdentity("4"), full: true, frame: fullFrame(4),
          comparison: "equal", difference: null,
        },
      ],
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: rewound }))).toMatchObject({
      ok: false, code: "invalid_argument", field: "worker.emissions[1].stream.seq",
    });
  });

  test("rejects an emission referencing an unknown segment", () => {
    const orphan = workerSection({
      emissions: [{
        segment_id: "seg-missing", emitted_at_ms: 1_700_000_000_001,
        stream: streamIdentity("1"), full: true, frame: fullFrame(1),
        comparison: "equal", difference: null,
      }],
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: orphan }))).toMatchObject({
      ok: false, field: "worker.emissions[0].segment_id",
    });
  });

  test("rejects a full frame whose viewport is not dense", () => {
    const sparse = workerSection({
      emissions: [{
        segment_id: "seg-1", emitted_at_ms: 1_700_000_000_001,
        stream: streamIdentity("1"), full: true,
        frame: { ...fullFrame(1), rows: 2 },
        comparison: "equal", difference: null,
      }],
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: sparse }))).toMatchObject({
      ok: false, field: "worker.emissions[0].frame.viewportRows",
    });
  });

  test("rejects raw offsets that are not decimal uint64 or run backwards", () => {
    // A bad single offset names that offset; only the cross-field ordering
    // invariant reports the pair, because no one field is the wrong one.
    const floated = workerSection({
      raw: [{
        segment_id: "seg-1", at_ms: 1, start_offset: 0, end_offset: "2", base64: "",
      }],
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: floated }))).toMatchObject({
      ok: false, code: "invalid_argument", field: "worker.raw[0].start_offset",
    });
    const rounded = workerSection({
      raw: [{
        segment_id: "seg-1", at_ms: 1, start_offset: "007", end_offset: "2", base64: "",
      }],
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: rounded }))).toMatchObject({
      ok: false, code: "invalid_argument", field: "worker.raw[0].start_offset",
    });
    const reversed = workerSection({
      raw: [{
        segment_id: "seg-1", at_ms: 1, start_offset: "9", end_offset: "2", base64: "",
      }],
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: reversed }))).toMatchObject({
      ok: false, code: "invalid_argument", field: "worker.raw[0].offsets",
    });
  });

  test("rejects a record array past the per-layer entry bound", () => {
    const flooded = workerSection({
      raw: Array.from({ length: TERMINAL_CAPTURE_LIMITS.layerEntries + 1 }, () => ({
        segment_id: "seg-1", at_ms: 1, start_offset: "0", end_offset: "0", base64: "",
      })),
    });
    expect(validateTerminalIncidentBundle(bundle({ worker: flooded }))).toMatchObject({
      ok: false, code: "resource_exhausted", field: "worker.raw",
    });
  });

  test("a rejection carries no terminal text", () => {
    const leaky = workerSection({
      emissions: [{
        segment_id: "seg-1", emitted_at_ms: 1_700_000_000_001,
        stream: streamIdentity("1"), full: true,
        frame: {
          ...fullFrame(1),
          viewportRows: [{
            index: 0,
            spans: [{ text: "sk-secret-value", fg: 256, bg: 256, flags: 0, columns: 0 }],
          }],
        },
        comparison: "equal", difference: null,
      }],
    });
    const result = validateTerminalIncidentBundle(bundle({ worker: leaky }));
    expect(result.ok).toBe(false);
    expect(JSON.stringify(result)).not.toContain("secret");
  });
});

describe("isDecimalUint64", () => {
  test("accepts exact 64-bit offsets and refuses lossy or padded forms", () => {
    expect(isDecimalUint64("0")).toBe(true);
    expect(isDecimalUint64("18446744073709551615")).toBe(true);
    expect(isDecimalUint64("18446744073709551616")).toBe(false);
    expect(isDecimalUint64("007")).toBe(false);
    expect(isDecimalUint64("-1")).toBe(false);
    expect(isDecimalUint64("1e3")).toBe(false);
    expect(isDecimalUint64(7 as unknown)).toBe(false);
  });
});

// Evidence-payload shape contract between the three capture layers.
//
// This exists because a shape disagreement between two producers cost a whole
// layer of evidence in production with nothing but an omission line to show
// for it: the browser nested its section under `browser`, the coordinator
// flattened its section onto the envelope, and the worker treated the envelope
// as the section — so `bundle.browser` validated as an envelope, failed as a
// section, and every capture silently shipped without browser evidence. Every
// suite that hand-fixtured its sections stayed green throughout.
//
// The rule pinned here: a payload is an envelope PLUS the layer's section
// nested under a member named for that layer. A flattened payload is rejected,
// and the rejection is at the envelope boundary where a bridge can report it.

import { describe, expect, test } from "bun:test";
import {
  TERMINAL_INCIDENT_SCHEMA,
  checkTerminalCaptureEnvelope,
  type TerminalBrowserSection,
  type TerminalCaptureBrowserPayload,
  type TerminalCaptureCommand,
  type TerminalCaptureCoordinatorPayload,
  type TerminalCaptureTrigger,
  type TerminalCoordinatorSection,
} from "../src/terminal-capture.ts";
import { validateTerminalIncidentBundle } from "../src/terminal-capture-validate.ts";

const SESSION = "22222222-2222-4222-8222-222222222222";
const CAPTURE = "33333333-3333-4333-8333-333333333333";
const RECORDING = "44444444-4444-4444-8444-444444444444";

const COMMAND: TerminalCaptureCommand = {
  action: "capture",
  session_id: SESSION,
  recording_id: RECORDING,
  capture_id: CAPTURE,
  reason: "history_identity",
  browser_evidence_json: "",
};

const TRIGGER: TerminalCaptureTrigger = {
  reason: "history_identity",
  origin: "browser",
  at_ms: 1_700_000_000_000,
  stream_id: "11111111-1111-4111-8111-111111111111",
  grid_epoch: "epoch:1",
  seq: "9",
  detail: "history_duplicate_index",
  occurrence_count: 3,
};

function header<L extends "browser" | "coordinator">(layer: L) {
  return {
    layer,
    captured_at_ms: 1_700_000_000_001,
    process: {
      layer,
      process_id: `${layer}-1`,
      git_sha: "abc1234",
      artifact_version: "2.0.0",
      wasm_identity: null,
      worker_fp: null,
      viewer_id: null,
      user_agent: null,
    },
    stream: null,
    geometry: null,
    dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
    omissions: [],
  } as const;
}

function browserSection(): TerminalBrowserSection {
  return {
    ...header("browser"),
    events: [],
    replica: null,
    trigger_state: null,
    pre_repair_state: null,
    post_repair_state: null,
    current_state: null,
  };
}

function coordinatorSection(): TerminalCoordinatorSection {
  return { ...header("coordinator"), records: [], snapshot: null, valid: true };
}

function browserPayload(): TerminalCaptureBrowserPayload {
  return {
    schema: TERMINAL_INCIDENT_SCHEMA,
    layer: "browser",
    capture_id: CAPTURE,
    recording_id: RECORDING,
    session_id: SESSION,
    trigger: TRIGGER,
    browser: browserSection(),
  };
}

function coordinatorPayload(): TerminalCaptureCoordinatorPayload {
  return {
    schema: TERMINAL_INCIDENT_SCHEMA,
    layer: "coordinator",
    capture_id: CAPTURE,
    recording_id: RECORDING,
    session_id: SESSION,
    coordinator: coordinatorSection(),
  };
}

describe("evidence payload nesting", () => {
  test("a nested browser payload yields its section and its trigger", () => {
    const checked = checkTerminalCaptureEnvelope(
      JSON.stringify(browserPayload()),
      { layer: "browser", command: COMMAND },
    );
    expect(checked.ok).toBe(true);
    if (!checked.ok) return;
    expect(checked.section.captured_at_ms).toBe(1_700_000_000_001);
    expect(checked.section.layer).toBe("browser");
    // The browser authors the trigger: it is the only layer that knows which
    // invariant fired and how many occurrences the latch collapsed.
    expect(checked.trigger).toMatchObject({
      detail: "history_duplicate_index",
      occurrence_count: 3,
    });
  });

  test("a nested coordinator payload yields its section", () => {
    const checked = checkTerminalCaptureEnvelope(
      JSON.stringify(coordinatorPayload()),
      { layer: "coordinator", command: COMMAND },
    );
    expect(checked.ok).toBe(true);
    if (!checked.ok) return;
    expect(checked.section.records).toEqual([]);
    expect(checked.trigger).toBeNull();
  });

  test("a FLATTENED payload is rejected at the envelope, naming the layer", () => {
    // The production defect: envelope fields spread onto the section. It looks
    // like a valid envelope and is useless as a section.
    const flattened = { ...browserPayload(), ...browserSection() } as unknown as Record<string, unknown>;
    delete flattened.browser;
    expect(checkTerminalCaptureEnvelope(
      JSON.stringify(flattened),
      { layer: "browser", command: COMMAND },
    )).toMatchObject({ ok: false, code: "evidence_malformed", field: "browser" });
  });

  test("a payload whose section member is not an object is rejected", () => {
    for (const section of [null, "browser", 7, []]) {
      const payload = { ...browserPayload(), browser: section };
      expect(checkTerminalCaptureEnvelope(
        JSON.stringify(payload),
        { layer: "browser", command: COMMAND },
      )).toMatchObject({ ok: false, field: "browser" });
    }
  });

  test("cross-session and cross-capture evidence is refused before any forward", () => {
    for (const [field, override] of [
      ["capture_id", { capture_id: RECORDING }],
      ["recording_id", { recording_id: CAPTURE }],
      ["session_id", { session_id: CAPTURE }],
    ] as const) {
      expect(checkTerminalCaptureEnvelope(
        JSON.stringify({ ...browserPayload(), ...override }),
        { layer: "browser", command: COMMAND },
      )).toMatchObject({ ok: false, code: "permission_denied", field });
    }
  });

  test("an envelope placed where a section belongs fails the bundle gate", () => {
    // The second half of the same defect: even if a bridge forwarded the
    // envelope, the write-side gate must refuse it rather than persist a
    // bundle whose browser section is an envelope.
    const bundle = {
      schema: TERMINAL_INCIDENT_SCHEMA,
      capture_id: CAPTURE,
      recording_id: RECORDING,
      session_id: SESSION,
      written_at_ms: 1_700_000_000_002,
      trigger: TRIGGER,
      coverage: {
        cell_replay: "complete", cell_replay_reasons: ["complete"],
        core_replay: "complete", core_replay_reasons: ["complete"],
        core_comparison: "complete", core_comparison_reasons: ["complete"],
      },
      browser: browserPayload(),
      coordinator: null,
      worker: null,
    };
    expect(validateTerminalIncidentBundle(bundle)).toMatchObject({
      ok: false,
      field: "browser.captured_at_ms",
    });
  });

  test("the properly unwrapped section passes the bundle gate", () => {
    const bundle = {
      schema: TERMINAL_INCIDENT_SCHEMA,
      capture_id: CAPTURE,
      recording_id: RECORDING,
      session_id: SESSION,
      written_at_ms: 1_700_000_000_002,
      trigger: TRIGGER,
      coverage: {
        cell_replay: "complete", cell_replay_reasons: ["complete"],
        core_replay: "complete", core_replay_reasons: ["complete"],
        core_comparison: "complete", core_comparison_reasons: ["complete"],
      },
      browser: browserSection(),
      coordinator: coordinatorSection(),
      worker: null,
    };
    expect(validateTerminalIncidentBundle(bundle).ok).toBe(true);
  });
});

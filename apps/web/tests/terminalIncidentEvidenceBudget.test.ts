// Browser evidence budget: how the 512 KiB UTF-8 ceiling is met without lying
// about what survived.
// Pure trimming over a synthetic payload — no coordinator client, no recorder
// lease — so this tier owns exactly one question: which evidence is sacrificed
// first, and is the export honest about it.

import { describe, expect, test } from "bun:test";
import {
  TERMINAL_CAPTURE_LIMITS,
  TERMINAL_INCIDENT_SCHEMA,
  utf8ByteLength,
  type TerminalBrowserPaintedState,
  type TerminalDomRow,
} from "@roost/protocol/terminal-capture";
import {
  fitBrowserEvidence,
  type TerminalIncidentBrowserPayload,
} from "../src/renderer/terminalIncidentCaptureEvidence.ts";

const SESSION = "22222222-2222-4222-8222-222222222222";
describe("terminal incident capture — evidence budget", () => {
  const LIMIT = TERMINAL_CAPTURE_LIMITS.browserEvidenceBytes;

  function domRows(count: number, chars: number): TerminalDomRow[] {
    return Array.from({ length: count }, (_, index) => ({
      order: index,
      index,
      columns: chars,
      fingerprint: index,
      text: "x".repeat(chars),
      span_count: 1,
    }));
  }

  function paintedState(
    phase: TerminalBrowserPaintedState["phase"],
    rows: number,
    chars: number,
    viewportRows: number,
  ): TerminalBrowserPaintedState {
    return {
      at_ms: 1,
      phase,
      apply_mode: "delta",
      canonical: null,
      committed: null,
      pending: null,
      painted_model_history: [],
      dom_history: domRows(rows, chars),
      dom_viewport: domRows(viewportRows, chars),
      gaps: [],
      cursor: { row: 0, col: 0, visible: true },
      scroll: { top: 0, height: 100, client_height: 50, row_height: 16 },
      reader: { intent: "live", reason: null, hold_mask: 0 },
      active: true,
      visible: true,
      omissions: [],
    };
  }

  function payloadWith(
    triggerRows: number,
    chars: number,
    viewportRows = 4,
  ): TerminalIncidentBrowserPayload {
    return {
      schema: TERMINAL_INCIDENT_SCHEMA,
      layer: "browser",
      capture_id: "capture-1",
      recording_id: "recording-1",
      session_id: SESSION,
      trigger: {
        reason: "history_identity",
        origin: "browser",
        at_ms: 1,
        stream_id: "stream-1",
        grid_epoch: "epoch-1",
        seq: "9",
        detail: "history_duplicate_index",
        occurrence_count: 1,
      },
      browser: {
        layer: "browser",
        captured_at_ms: 1,
        process: {
          layer: "browser",
          process_id: "process-1",
          git_sha: "sha",
          artifact_version: "2.0.0",
          wasm_identity: null,
          worker_fp: null,
          viewer_id: "tab-1",
          user_agent: "test",
        },
        stream: null,
        geometry: null,
        dropped: { records: 0, bytes: 0, rows: 0, raw_bytes: 0, samples: 0 },
        omissions: [],
        events: Array.from({ length: TERMINAL_CAPTURE_LIMITS.layerEntries }, (_, index) => ({
          at_ms: index,
          kind: "render_applied" as const,
          stream: null,
          apply_mode: "delta" as const,
          detail: "x".repeat(512),
        })),
        replica: { note: "x".repeat(2048) },
        trigger_state: paintedState("pre_history_insert", triggerRows, chars, viewportRows),
        pre_repair_state: paintedState("pre_apply", triggerRows, chars, viewportRows),
        post_repair_state: paintedState("post_reconcile", triggerRows, chars, viewportRows),
        current_state: paintedState("current", triggerRows, chars, viewportRows),
      },
    };
  }

  test("trimming drops replay segments first and keeps the trigger snapshot", () => {
    const payload = payloadWith(120, 900);
    const fitted = fitBrowserEvidence(payload);

    expect(fitted.partial).toBe(true);
    expect(utf8ByteLength(fitted.json)).toBeLessThanOrEqual(LIMIT);
    const parsed = JSON.parse(fitted.json);
    expect(parsed.browser.trigger_state).not.toBeNull();
    expect(parsed.browser.events).toHaveLength(0);
    const omitted = parsed.browser.omissions.map((entry: { name: string }) => entry.name);
    expect(omitted).toContain("events");
    // The untrimmed payload is still available for a local export.
    expect(payload.browser.events).toHaveLength(TERMINAL_CAPTURE_LIMITS.layerEntries);
    expect(payload.browser.trigger_state?.dom_history).toHaveLength(120);
  });

  test("a trigger snapshot that cannot fit becomes a metadata-only partial", () => {
    // Visible rows are never dropped, so a viewport this large cannot fit.
    const payload = payloadWith(200, 2000, 400);
    const fitted = fitBrowserEvidence(payload);

    expect(fitted.partial).toBe(true);
    expect(utf8ByteLength(fitted.json)).toBeLessThanOrEqual(LIMIT);
    const parsed = JSON.parse(fitted.json);
    expect(parsed.browser.trigger_state).toBeNull();
    expect(parsed.browser.current_state).toBeNull();
    expect(parsed.trigger.detail).toBe("history_duplicate_index");
    const omitted = parsed.browser.omissions.map((entry: { name: string }) => entry.name);
    expect(omitted).toContain("trigger_state");
    // Metadata-only on the wire, whole evidence still held for local export.
    expect(payload.browser.trigger_state).not.toBeNull();
    expect(payload.browser.trigger_state?.dom_viewport).toHaveLength(400);
  });

  test("evidence inside the budget reports itself complete", () => {
    const base = payloadWith(2, 10);
    const payload: TerminalIncidentBrowserPayload = {
      ...base,
      browser: { ...base.browser, events: [], replica: { note: "small" } },
    };
    const fitted = fitBrowserEvidence(payload);

    expect(fitted.partial).toBe(false);
    expect(JSON.parse(fitted.json).browser.trigger_state).not.toBeNull();
  });
});

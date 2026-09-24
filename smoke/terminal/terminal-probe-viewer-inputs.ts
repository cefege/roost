// Validated reader for the coordinator's per-view geometry inputs, carried by
// the terminal stream probe's terminal_control block. Specs use it to prove the
// effective size IS the minimum of the inputs that constrain it, rather than
// recomputing the policy in the test. Lives beside terminal-probe-helpers.ts
// (which owns the shared record/integer guards) because that file is at its
// line cap.

import { minimumTerminalGeometry, type TerminalGeometry } from "@roost/protocol/viewport";
import type { TerminalStreamProbe } from "../../apps/web/src/smoke/smoke.ts";
import { nonNegativeInteger, unknownRecord } from "./terminal-probe-helpers.ts";

export interface CoordinatorTerminalViewerInput {
  fingerprint: string;
  viewId: string;
  cols: number;
  rows: number;
  parked: boolean;
  constrains: boolean;
}

/** Null means the coordinator layer or its terminal_control block is missing;
 *  a present-but-malformed field throws instead of reading as "no viewers". */
export function coordinatorTerminalViewerInputs(
  probe: TerminalStreamProbe,
): readonly CoordinatorTerminalViewerInput[] | null {
  const control = probe.coord?.terminal_control;
  if (!control) return null;
  const inputs = control.viewer_inputs;
  if (inputs === null || inputs === undefined) return null;
  if (!Array.isArray(inputs)) {
    throw new Error("coordinator viewer inputs were not an array");
  }
  return inputs.map((entry, index) => {
    const record = unknownRecord(entry);
    if (!record) throw new Error(`coordinator viewer input ${index} was not an object`);
    const fingerprint = record.fingerprint;
    const viewId = record.viewId;
    if (typeof fingerprint !== "string" || fingerprint.length === 0) {
      throw new Error(`coordinator viewer input ${index} omitted its fingerprint`);
    }
    if (typeof viewId !== "string" || viewId.length === 0) {
      throw new Error(`coordinator viewer input ${index} omitted its view ID`);
    }
    if (typeof record.parked !== "boolean" || typeof record.constrains !== "boolean") {
      throw new Error(`coordinator viewer input ${index} had non-boolean membership flags`);
    }
    return {
      fingerprint,
      viewId,
      cols: nonNegativeInteger(record.cols, `coordinator viewer input ${index} columns`),
      rows: nonNegativeInteger(record.rows, `coordinator viewer input ${index} rows`),
      parked: record.parked,
      constrains: record.constrains,
    };
  });
}

/** The size the coordinator's own reported inputs demand. Uses the single
 *  shared SCD implementation so a spec never carries a second copy of it. */
export function coordinatorConstrainedGeometry(
  probe: TerminalStreamProbe,
): TerminalGeometry | null {
  const inputs = coordinatorTerminalViewerInputs(probe);
  if (inputs === null) return null;
  return minimumTerminalGeometry(inputs.filter((input) => input.constrains));
}

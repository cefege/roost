#!/usr/bin/env bun
// scripts/replay-terminal-incident.ts — replay ONE terminal incident bundle and
// attribute a duplicated/mis-painted terminal to the first layer whose evidence
// actually diverges. Reads a local `.json.gz` written by a worker's capture
// storage; validates it with @roost/shared/terminal-capture-validate before
// touching it; compares layers with the production canonical view and folds
// with the production cell pipeline (scripts/replay-terminal-incident-layers.ts).
//
// The console report is CONTENT-FREE: identities, coordinates, field names,
// coverage and counts only. Row and byte detail stays in the owner-only bundle.
// Captured bytes are written to a terminal core; they are never executed.
//
// Run: bun scripts/replay-terminal-incident.ts <bundle.json.gz>

import { readFileSync, statSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import {
  TERMINAL_CAPTURE_LIMITS,
  type TerminalBrowserPaintedState,
  type TerminalCanonicalDifference,
  type TerminalCaptureCoverageReport,
  type TerminalIncidentBundle,
} from "../apps/shared/src/terminal-capture.ts";
import {
  validateTerminalIncidentBundle,
} from "../apps/shared/src/terminal-capture-validate.ts";
import {
  browserCheckpoints,
  coordinatorCheckpoints,
  matchLayers,
  replayRawToCore,
  workerCoreCheckpoints,
  workerFoldCheckpoints,
  type CheckpointSet,
  type MatchReport,
  type RawReplayResult,
} from "./replay-terminal-incident-layers.ts";
import {
  rendererReport,
  type RendererFinding,
  type RendererReport,
} from "./replay-terminal-incident-paint.ts";

type AttributedLayer =
  | "worker_emission"
  | "coordinator_admission"
  | "browser_admission"
  | "renderer_reconciliation"
  | "none"
  | "none_of_the_covered_layers"
  | "unresolved_application_or_core";

function fail(message: string): never {
  console.error(`replay-terminal-incident: ${message}`);
  process.exit(2);
}

function loadBundle(path: string): TerminalIncidentBundle {
  if (!path.endsWith(".json.gz")) fail("bundle path must end in .json.gz");
  const stat = statSync(path);
  if (!stat.isFile()) fail("bundle path is not a file");
  if (stat.size > TERMINAL_CAPTURE_LIMITS.bundleBytes) {
    fail(`compressed bundle exceeds ${TERMINAL_CAPTURE_LIMITS.bundleBytes} bytes`);
  }
  let json: string;
  try {
    // maxOutputLength is the decompression bound: a 32 MiB ceiling on the
    // UNCOMPRESSED payload is what keeps a hostile or corrupt archive from
    // deciding this process's memory footprint.
    json = gunzipSync(readFileSync(path), {
      maxOutputLength: TERMINAL_CAPTURE_LIMITS.bundleBytes,
    }).toString("utf8");
  } catch {
    fail("bundle could not be decompressed within the uncompressed size limit");
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    fail("bundle is not valid JSON");
  }
  const validation = validateTerminalIncidentBundle(parsed);
  if (!validation.ok) fail(`bundle rejected: ${validation.code} at ${validation.field}`);
  return validation.bundle;
}

function describeCoverage(coverage: TerminalCaptureCoverageReport): string {
  return [
    `cell_replay=${coverage.cell_replay}(${coverage.cell_replay_reasons.join(",")})`,
    `core_replay=${coverage.core_replay}(${coverage.core_replay_reasons.join(",")})`,
    `core_comparison=${coverage.core_comparison}(${coverage.core_comparison_reasons.join(",")})`,
  ].join(" ");
}

function describeDifference(difference: TerminalCanonicalDifference): string {
  return difference.kind === "state"
    ? `field=${difference.field} left=${difference.left} right=${difference.right}`
    : `row=${difference.row} column=${difference.column} field=${difference.field}`
      + ` left=${difference.left} right=${difference.right}`;
}

function describeMatch(name: string, report: MatchReport): string {
  const verdict = report.difference === null
    ? report.matched === 0 ? "unavailable" : "equal"
    : "different";
  const detail = report.difference === null
    ? ""
    : ` stream=${report.difference.stream.stream_id}`
      + ` epoch=${report.difference.stream.grid_epoch}`
      + ` seq=${report.difference.stream.seq}`
      + ` ${describeDifference(report.difference.difference)}`;
  return `${name}: ${verdict} matched=${report.matched} unmatched=${report.unmatched}${detail}`;
}

function describeFinding(finding: RendererFinding): string {
  const coordinates = [
    finding.absoluteRow === null ? null : `absolute_row=${finding.absoluteRow}`,
    finding.viewportRow === null ? null : `viewport_row=${finding.viewportRow}`,
    finding.domOrder === null ? null : `dom_order=${finding.domOrder}`,
  ].filter((part): part is string => part !== null);
  return `${finding.kind} ${coordinates.join(" ")}`.trimEnd();
}

/** Attribution order follows the data plane: whichever layer is the FIRST to
 *  disagree with the layer upstream of it owns the defect. A layer with no
 *  matched checkpoint is unavailable coverage and attributes nothing.
 *
 *  `none` therefore means something stronger than "no difference found": it
 *  means every layer was actually compared and agreed. When a layer had no
 *  coverage the verdict is `none_of_the_covered_layers`, because reporting
 *  `none` would exonerate a layer nobody looked at — and the renderer, the
 *  layer most often uncovered, is exactly where a duplicated footer lives. */
function attribute(
  coreVsFold: MatchReport,
  foldVsCoordinator: MatchReport,
  coordinatorVsBrowser: MatchReport,
  renderer: RendererReport,
  rawReplay: RawReplayResult,
): AttributedLayer {
  if (coreVsFold.difference !== null) return "worker_emission";
  if (foldVsCoordinator.difference !== null) return "coordinator_admission";
  if (coordinatorVsBrowser.difference !== null) return "browser_admission";
  if (renderer.findings.length > 0) return "renderer_reconciliation";
  const everyLayerCovered = coreVsFold.matched > 0
    && foldVsCoordinator.matched > 0
    && coordinatorVsBrowser.matched > 0
    && (renderer.historyStates > 0 || renderer.viewportStates > 0);
  if (!everyLayerCovered) return "none_of_the_covered_layers";
  // Every layer was compared and agreed. If an exact replay from this core's
  // initialization also reproduces the same screen, the duplication was already
  // in the bytes the application wrote — and nothing in this bundle can
  // separate "the app drew it twice" from "the core interpreted it twice"
  // without an independently verified cursor-addressed trace.
  if (rawReplay.status === "complete" && rawReplay.difference === null) {
    return "unresolved_application_or_core";
  }
  return "none";
}

function browserStates(
  bundle: TerminalIncidentBundle,
): (TerminalBrowserPaintedState | null)[] {
  const browser = bundle.browser;
  if (!browser) return [];
  return [
    browser.trigger_state,
    browser.pre_repair_state,
    browser.post_repair_state,
    browser.current_state,
  ];
}

function emptyCheckpoints(): CheckpointSet {
  return { checkpoints: new Map(), reasons: ["layer_unavailable"] };
}

const args = process.argv.slice(2);
if (args.length !== 1 || args[0] === undefined) {
  fail("expects exactly one argument: the path to one <capture>.json.gz bundle");
}

const bundle = loadBundle(args[0]);
const worker = bundle.worker;
const coordinator = bundle.coordinator;

const foldSet = worker ? workerFoldCheckpoints(worker) : emptyCheckpoints();
const coreSet = worker ? workerCoreCheckpoints(worker) : emptyCheckpoints();
const coordinatorSet = coordinator ? coordinatorCheckpoints(coordinator) : emptyCheckpoints();
const states = browserStates(bundle);
const browserSet = browserCheckpoints(states);

const coreVsFold = matchLayers(coreSet, foldSet);
const foldVsCoordinator = matchLayers(foldSet, coordinatorSet);
const coordinatorVsBrowser = matchLayers(coordinatorSet, browserSet);
const renderer = rendererReport(states);
const rawReplay = worker
  ? await replayRawToCore(worker, foldSet)
  : { status: "unavailable", reason: "layer_unavailable", bytes: 0, resizes: 0, difference: null, comparedAgainst: "none" } as RawReplayResult;

const layer = attribute(coreVsFold, foldVsCoordinator, coordinatorVsBrowser, renderer, rawReplay);

console.log(`capture ${bundle.capture_id} recording ${bundle.recording_id} session ${bundle.session_id}`);
console.log(`written_at ${new Date(bundle.written_at_ms).toISOString()}`);
for (const section of [bundle.worker, bundle.coordinator, bundle.browser]) {
  if (!section) continue;
  const identity = section.process;
  console.log(
    `build ${identity.layer}: sha=${identity.git_sha} artifact=${identity.artifact_version}`
    + ` process=${identity.process_id}`
    + ` wasm=${identity.wasm_identity ?? "n/a"}`
    + ` worker_fp=${identity.worker_fp ?? "n/a"}`
    + ` viewer=${identity.viewer_id ?? "n/a"}`,
  );
}
for (const name of ["worker", "coordinator", "browser"] as const) {
  if (bundle[name] === null) console.log(`section ${name}: unavailable`);
}
console.log(`coverage ${describeCoverage(bundle.coverage)}`);
console.log(
  `trigger reason=${bundle.trigger.reason} origin=${bundle.trigger.origin}`
  + ` stream=${bundle.trigger.stream_id ?? "n/a"} epoch=${bundle.trigger.grid_epoch ?? "n/a"}`
  + ` seq=${bundle.trigger.seq ?? "n/a"} detail=${bundle.trigger.detail ?? "n/a"}`
  + ` occurrences=${bundle.trigger.occurrence_count}`,
);
console.log(
  `checkpoints core=${coreSet.checkpoints.size} fold=${foldSet.checkpoints.size}`
  + ` coordinator=${coordinatorSet.checkpoints.size} browser=${browserSet.checkpoints.size}`,
);
console.log(describeMatch("core_vs_worker_fold", coreVsFold));
console.log(describeMatch("worker_fold_vs_coordinator", foldVsCoordinator));
console.log(describeMatch("coordinator_vs_browser_replica", coordinatorVsBrowser));
// `equal` must mean "compared and agreed". With no history state retained and
// no viewport at a matching clock, nothing was checked — reporting that as
// `equal` would read as a verified DOM and exonerate the renderer for free.
const rendererVerdict = renderer.findings.length > 0
  ? "different"
  : renderer.historyStates === 0 && renderer.viewportStates === 0
    ? "unavailable"
    : "equal";
console.log(
  `painted_model_vs_dom: ${rendererVerdict}`
  + ` history_states=${renderer.historyStates}`
  + ` viewport_states=${renderer.viewportStates}`
  + ` viewport_skipped_other_clock=${renderer.viewportSkipped}`
  + ` findings=${renderer.findings.length}`,
);
for (const finding of renderer.findings.slice(0, 8)) {
  console.log(`  finding ${describeFinding(finding)}`);
}
console.log(
  `raw_core_replay: ${rawReplay.status} reason=${rawReplay.reason}`
  + ` bytes=${rawReplay.bytes} resizes=${rawReplay.resizes}`
  + ` compared_against=${rawReplay.comparedAgainst}`
  + (rawReplay.difference === null ? "" : ` ${describeDifference(rawReplay.difference)}`),
);
for (const section of [bundle.worker, bundle.coordinator, bundle.browser]) {
  for (const omission of section?.omissions ?? []) {
    console.log(
      `omitted ${section!.layer}.${omission.name} kind=${omission.kind}`
      + ` reason=${omission.reason} count=${omission.dropped_count}`
      + ` bytes=${omission.dropped_bytes}`
      + (omission.range === null ? "" : ` range=${omission.range.start}..${omission.range.end}`),
    );
  }
}
console.log(`first_divergent_layer ${layer}`);

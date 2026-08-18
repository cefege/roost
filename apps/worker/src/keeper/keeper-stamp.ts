// KEEPER_BUILD_STAMP — a code-version stamp for the multiplexed keeper.
//
// The keeper is spawned detached and OUTLIVES a worker deploy, so a
// behavior-only change ships dormant until the keeper process is replaced.
// KEEPER_PROTOCOL_VERSION (protocol.ts) only catches WIRE changes; this
// stamp catches CODE changes so a stale keeper can be surfaced + refreshed.
//
// It is a content hash of the keeper's OWN runtime source files — NOT the repo
// git sha (which over-triggers: every unrelated SPA/coord commit would force a
// DESTRUCTIVE keeper replacement) and NOT process.env.GIT_SHA (stale after a
// plain rsync deploy — the coord DriftBadge bug, coord/git-sha.ts). Only a
// change to a file below moves the stamp. Worker + keeper import this module
// and compute it identically from disk; the keeper freezes its value at spawn
// and echoes it in HelloResp, the freshly-booted worker computes the current
// value and compares (multiplexed-client.ts probeKeeperCompatible).

import { readFileSync } from "node:fs";
import { join } from "node:path";

// The files the keeper PROCESS actually runs. multiplexed-client.ts is
// worker-side (not loaded by the keeper) and is deliberately excluded — a
// client-only change must not flag every keeper stale.
const KEEPER_SOURCE_FILES = [
  "multiplexed-main.ts", "protocol.ts", "histfile.ts",
  // The keeper protocol module was renamed and split by message family
  // 2026-08-18; protocol.ts is now only the version record + re-exports, so the
  // family modules holding the actual keeper-side codec must be listed too or a
  // wire-codec change stops moving the stamp.
  "protocol-envelope.ts", "protocol-io.ts", "protocol-terminal.ts",
  // multiplexed-main.ts was split 2026-07-10; these siblings hold keeper
  // runtime code (log/types/reaping/frame-dispatch) that must stay covered
  // so a keeper behavior change in any of them still moves the stamp.
  "keeper-log.ts", "keeper-types.ts", "keeper-process-reap.ts", "keeper-frame-handler.ts",
  // keeper-frame-handler.ts imports the worker's fixed-capacity byte ring for
  // its per-channel outRing, so a change there changes keeper RUNTIME
  // behaviour. A detached keeper outlives a worker deploy: without this entry
  // a stale keeper keeps running the old grow-and-slice copy and reports
  // itself fresh.
  "../session-scrollback-ring.ts", "../shell-spec.ts",
  "../../../shared/src/platform.ts", "../../../shared/src/native-path.ts",
  "../../../shared/src/local-endpoint.ts", "../../../shared/src/windows-helper.ts",
];

function computeKeeperBuildStamp(): string {
  try {
    const hasher = new Bun.CryptoHasher("sha256");
    for (const name of KEEPER_SOURCE_FILES) {
      hasher.update(readFileSync(join(import.meta.dir, name)));
    }
    return hasher.digest("hex").slice(0, 12);
  } catch {
    // Read failure (unexpected deploy layout) → stable sentinel. Worker AND
    // keeper both get "unknown" → equal → no false staleness, just no signal.
    return "unknown";
  }
}

export const KEEPER_BUILD_STAMP = computeKeeperBuildStamp();

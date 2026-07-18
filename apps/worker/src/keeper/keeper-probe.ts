// Worker-side probe of a possibly-stale keeper socket. Extracted from
// multiplexed-client.ts; both symbols are re-exported from there so callers
// keep the same import path.

import { createConnection } from "node:net";
import { existsSync } from "node:fs";
import {
  MuxFrameType,
  KEEPER_PROTOCOL_VERSION,
  encodeMuxFrame, decodeMuxFrames,
} from "./protocol-v2.ts";

/** Probe a possibly-stale keeper socket for protocol compatibility.
 *  Returns true iff:
 *    - sock file is absent (nothing to probe; caller will spawn fresh), OR
 *    - sock accepts a connection AND replies to Hello with a
 *      HelloResp whose version matches KEEPER_PROTOCOL_VERSION
 *      within `timeoutMs`.
 *  Returns false on:
 *    - ECONNREFUSED (dead unlink-after-crash sock), OR
 *    - connect/Hello-write/HelloResp timeout (old keeper that pre-dates
 *      Hello silently drops the frame), OR
 *    - HelloResp.version !== KEEPER_PROTOCOL_VERSION (deployed-against
 *      a wire change since last keeper boot).
 *  Replaces the pre-rewrite "always kill at worker boot" rule —
 *  same-version survivor keepers now resume their channels intact, so
 *  worker restarts (LaunchAgent kickstart, CoordLink reconnect storm
 *  before phase-pathl idleTimeout fix) no longer wipe every running
 *  shell. See apps/worker/src/main.ts::killStaleKeeper for the only
 *  remaining call site. */
/** Result of a keeper Hello probe. `compatible` is the WIRE-version verdict
 *  (the only thing that gates killStaleKeeper). `keeperStamp` is the survivor's
 *  reported KEEPER_BUILD_STAMP — code-freshness, surfaced but NOT gated on
 *  (undefined = pre-stamp keeper or no keeper). */
export interface KeeperProbeResult {
  compatible: boolean;
  keeperStamp?: string;
}

export async function probeKeeperCompatible(
  sockPath: string,
  timeoutMs: number = 800,
): Promise<KeeperProbeResult> {
  if (!existsSync(sockPath)) return { compatible: true };
  return new Promise<KeeperProbeResult>((resolve) => {
    let resolved = false;
    const done = (ok: boolean, keeperStamp?: string) => {
      if (resolved) return;
      resolved = true;
      try { probe.destroy(); } catch { /* ignore */ }
      resolve({ compatible: ok, keeperStamp });
    };
    const probe = createConnection(sockPath);
    let rxBuf = Buffer.alloc(0) as Buffer;
    const timer = setTimeout(() => done(false), timeoutMs);
    probe.once("error", () => { clearTimeout(timer); done(false); });
    probe.once("connect", () => {
      probe.write(encodeMuxFrame(
        MuxFrameType.Hello, 0,
        JSON.stringify({ version: KEEPER_PROTOCOL_VERSION }),
      ));
    });
    probe.on("data", (chunk: Buffer | Uint8Array) => {
      rxBuf = Buffer.concat([rxBuf, Buffer.from(chunk)]);
      const { frames } = decodeMuxFrames(rxBuf);
      for (const f of frames) {
        if (f.type !== MuxFrameType.HelloResp) continue;
        clearTimeout(timer);
        try {
          const v = JSON.parse(f.payload.toString("utf8")) as { version?: number; build?: string };
          done(v.version === KEEPER_PROTOCOL_VERSION, v.build);
        } catch { done(false); }
        return;
      }
    });
  });
}

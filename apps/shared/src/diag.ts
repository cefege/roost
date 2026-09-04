// Diagnostic facade. ONE function: `diag(evt, kv)`. Single JSON line
// per call with shape:
//   {ts, level:"info", target:"diag", evt, mono_ns, ...kv}
// DEFAULT: OFF in production (opt in with ROOST_DIAG=1 (Bun) /
// localStorage.roostDiag='1' (SPA)). The firehose is a debugging tool —
// leaving it on produces ~1 GB/day of *.out.log (coord ~21% CPU).
// `roost doctor` reads *.err.log (Tier-1 signals), NOT the firehose.
//
// All callsites grep-correlatable via the `evt` token, `trace_id`
// (request-scoped, from connect interceptor / SPA RPC stamp) and
// `session_trace_id` (session-scoped, regenerated at session.spawn).
//
// Target = "diag" so the operational `log` facade keeps its own
// namespace. Grep:
//   rg '"target":"diag"' ~/Library/Logs/RoostCoord/main.out.log
//
// No-op when disabled — function pointer is swapped at module load.

import { log } from "./log.ts";

export type DiagKv = Record<string, unknown>;

// Platform-appropriate monotonic ns. Bun: Bun.nanoseconds() — true ns.
// Browser: performance.now() in ms, scaled. Used as a per-process
// tiebreak when wall-clock `ts` collides at sub-ms.
function makeMonoNs(): () => number {
  // Bun runtime (worker + coord)
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const bunGlobal = (globalThis as any).Bun;
  if (bunGlobal && typeof bunGlobal.nanoseconds === "function") {
    return () => Number(bunGlobal.nanoseconds());
  }
  // Browser
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return () => Math.round(performance.now() * 1e6);
  }
  return () => 0;
}
const monoNs = makeMonoNs();

// Enabled gate. Coord + worker read process.env at module load.
// Browser reads localStorage at module load. Cannot be toggled at
// runtime without page refresh / worker restart — by design (keeps
// the hot path branch-free once decided).
function readEnabled(): boolean {
  // DEFAULT: OFF. Opt IN with ROOST_DIAG=1 (Bun env) /
  // localStorage.roostDiag='1' (SPA). The firehose left on is the
  // documented trap (coord ~21% CPU + GBs/day of *.out.log);
  // docs/FAILURE-INDEX.md.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const proc = (globalThis as any).process;
  if (proc && proc.env && proc.env.ROOST_DIAG === "1") return true;
  if (typeof localStorage !== "undefined") {
    try { if (localStorage.getItem("roostDiag") === "1") return true; } catch { /* SSR or sandboxed */ }
  }
  return false;
}
const DIAG_ENABLED = readEnabled();

// Optional sink override. SPA installs a batched coord-shipping sink
// in apps/web/src/lib/diag.ts so events leave the browser. Coord +
// worker leave this null and emit via `log.info` → stdout → launchd.
type DiagSink = (record: Record<string, unknown>) => void;
let _sink: DiagSink | null = null;
export function setDiagSink(sink: DiagSink | null): void { _sink = sink; }

// The hot path. Branch on DIAG_ENABLED at the top so a disabled
// callsite is one boolean test + return.
function emitEnabled(evt: string, kv: DiagKv): void {
  const record: Record<string, unknown> = {
    evt,
    mono_ns: monoNs(),
    ...kv,
  };
  if (_sink) {
    _sink(record);
    return;
  }
  log.info("diag", evt, record);
}
function emitDisabled(_evt: string, _kv: DiagKv): void { /* no-op */ }

/**
 * Emit ONE diagnostic event. Default OFF; enabled when ROOST_DIAG === "1"
 * (Bun) or localStorage.roostDiag === "1" (browser).
 *
 * Grep tokens (snake_case, fixed namespace):
 *   viewport.{claim,withdraw,recompute,reap,coord_bump,spa_claim,spa_withdraw}
 *   scrollback.{fetch_start,fetch_done,gap,prepend,refetch_resize,serve,alt_flip}
 *   bytes.{chunk,write,write_throw,up_send,up_relay,up_recv,up_pty_write,up_retry,up_dropped}
 *   resize.{fit_measure,wterm_resize,pty_signal,suppress_block,wterm_core}
 *   focus.{win,force,active_class}   (force: kv.landed=false ⇒ "can't input")
 *   cell.{claim,emit,apply,offline_retry}  (cell-mode render path; seq, cursor_row/col/vis, full)
 *   projection.{pick,divergence}
 *   deck.{visibility_show,visibility_hide,mount,unmount}
 *   session.{spawn,close,attach,detach,snapshot}
 *   diag.{snapshot,corruption_signal,byte_dump_written}
 *   app.{freeze,resume,pageshow}
 *   auth.{key_first_boot}
 *   spa.{uncaught}
 *
 * Tier-1 always-on anomalies use `signal()` (below), NOT diag() — see SignalKind.
 */
export const diag: (evt: string, kv: DiagKv) => void =
  DIAG_ENABLED ? emitEnabled : emitDisabled;

export function isDiagEnabled(): boolean { return DIAG_ENABLED; }

// ─── Tier-1 signal channel ─────────────────────────────────────────────────
// ALWAYS on (independent of ROOST_DIAG), low-volume, genuine anomalies only.
// Bun → log.warn("signal", kind, kv) → stderr → *.err.log. SPA → an
// always-installed signal sink (apps/web/src/lib/diag.ts) that ships to coord.
// This is the daily-review channel `roost doctor` reads; keep callsites rare or
// rely on the per-kind cooldown below. Context-level detail stays on diag().
export type SignalKind =
  | "spa.uncaught"
  | "spa.chunk_reload"         // SPA self-healed a stale-chunk load after a redeploy (kv.msg,attempt). A BURST = deploys leaving tabs broken, or dist served inconsistently
  | "diag.corruption_signal"   // SPA anomaly detectors (kv.kind = the detector)
  | "tab.duplicate_identity_rotated" // duplicated browser tab inherited sessionStorage identity; newcomer rotated before opening authenticated transports
  | "auth.key_evicted"
  | "auth.relogin_401"
  | "input.drop_burst"
  | "reconnect.give_up"
  | "keeper.died"               // keeper subprocess exited → all PTYs lost; worker respawns + reconciles sessions
  | "keeper.degraded"           // keeper alive but births dead PTYs (spawn ok, no I/O) — emit_no_session burst; fix = fresh keeper
  | "keeper.degraded_unrecoverable" // restart budget exhausted (≥N restarts/window) — restarting again just re-SIGTERMs live PTYs; STOP + alert
  | "keeper.dead_birth"         // a single spawn → instant zero-byte exit (head_seq===0, <2s); N within window → keeper.degraded self-heal
  | "keeper.restart_degraded"   // worker force-restarted a degraded survivor keeper (self-heal; grace-gated to avoid loops)
  | "spawn.no_ack"              // keeper never acked a Spawn frame within timeout → session hangs (degraded/wedged keeper)
  | "spawn.agent_launch_dropped" // auto/forced agent launch input was rejected by terminal admission (Sync not connected/subscribed yet) — the PTY opens with no agent and nothing said why; kv names reason. One legitimate attempt exists; there is no retry path
  | "scrollback.gap"            // ring rolled past lastSeq → silent history hole on resume (observability, NOT a band-aid trigger)
  | "scrollback.replay_bound"   // a core rebuild could not reproduce the history the core it replaced still held, and/or its monotonic origin pin CLAMPED — the "history shrank / mis-spliced after a resize" class. A rebuild replays a FIXED byte ring, so once that ring no longer reaches as far back as the old core's line ring the history floor silently JUMPS; kv names the pin's before/after values, how many rows the replay could not reach, and whether the clamp fired. The one moment sbOrigin's correctness is in doubt, so it reports even though the rebuild itself succeeded
  | "terminal.gate_over_budget"  // worker cell-emission gate outlived the keeper command budget: a resize transaction (or repair) is stalling frames, so kv names the gate, its monotonic age, the transaction phase, and the captured byte count
  | "terminal.core_failed"      // in-place core resize/recovery trapped; the stream is fail-closed and later PTY bytes stay in ordered recovery records until adoption
  | "terminal.invalid_frame"    // worker canonical full exceeded structural/chunk limits and cannot establish a baseline for the stream
  | "terminal.screen_capacity" // coordinator replica bounds rejected a newly completing screen without evicting an active session
  | "terminal.snapshot_encode_failed" // coordinator could not encode a canonical cached snapshot for incremental recipient delivery
  | "terminal.stream_result_mismatch" // worker stream result did not match the coordinator lane's addressed stream or desired payload
  | "terminal.stream_invariant_failure" // worker classified a coordinator-produced stream request as structurally invalid
  | "terminal.sync_output_cap"   // an application opened a DEC 2026 synchronized-output frame and did not close it inside either ceiling, so the worker force-emitted the withheld cell frame and stopped suppressing that generation; kv names which cap tripped, the generation, the hold's monotonic age and how many frames it withheld
  | "terminal.hyperlink_saturated" // the core's fixed OSC 8 link table filled up, so every NEW distinct hyperlink this session emits silently renders as PLAIN TEXT (no error, no missing output — links just stop appearing); kv names capacity/used/rejected. Fires once per false→true flip, and the table resets on a core rebuild
  | "terminal.unhandled_sequence" // the terminal core's dispatcher IGNORED an escape sequence the application sent — the "renders wrong in Roost, fine in iTerm" class (e.g. DA1 `CSI c`, DECSCUSR `CSI Ps SP q`); kv names final/private/param_count/params plus ring_full+dropped. ONE line per distinct sequence per core instance (a rebuild reports afresh), per-channel cooldown. Partial detector: the core never logs unhandled OSC (other than 0/2/8) or unimplemented DECSET/DECRST modes
  | "voice.ws_failed"
  | "voice.mic_failed"          // mic capture never started (getUserMedia denied/busy/absent, or the pipeline was torn down mid-start) — the failure class the Deepgram WS signal is blind to
  | "voice.dictation_empty"     // a recording ended with an EMPTY transcript and no error anywhere; kv names the stage that went quiet (frames=0 dead audio graph / peak=0 device silence / chunks>0 results=0 Deepgram never answered / results>0 chars=0 it heard nothing) plus build+ua, because "the mic does nothing" is otherwise unfalsifiable after the tab closes
  | "nav.safety_net_redirect" // MainPane dead-route safety net navigated away from a terminal route (kv.reason=gone|stale-deeplink, kv.target). Live-session bounce = a resolution bug to chase from kv.sid.
  | "perf.longtask_stall"       // SPA main-thread task ≥ freeze threshold; kv carries the leak-watch accumulator snapshot (per-session map sizes, dom_nodes, heap_mb, uptime) at stall time → names days-long-uptime bloat vs a transient
  // ─── Coverage-sweep additions (coord Tier-1, worker transport/lifecycle, deploy) ───
  | "bytes.drop_unmapped"       // coord byte-hub dropped PTY output/cell/status for a channel with no session mapping (burst = real output/history loss, not the open-race)
  | "cell.announce_barrier_drop" // coord's announced-channel barrier abandoned buffered worker frames; a mapped terminal stream is invalidated and requests one full snapshot
  | "bytes.metadata_loss"       // coord dropped announced binary PTY frames: a later cell snapshot recreates the grid (hyperlinks included — they ride the cells) but NOT that channel's one-time OSC 0/2 title
  | "sync.backfill_failed"      // coord reconnect backfill query threw; live stream continued → SPA split-brain
  | "sync.backfill_truncated"   // coord backfill hit the getEventsSince row cap → events silently skipped
  | "sync.queue_overflow"       // coord Sync per-stream queue crossed high-water → slow subscriber / runaway producer
  | "sync.auth_rejected"        // coord rejected a browser Sync WS upgrade (jwt invalid / missing token); kv.reason
  | "sync.ws_frame_dropped"     // coord's ws.send returned 0 = the frame was DROPPED, not merely backpressured. A cell frame lost here is what the SPA's cell.seq_gap then recovers from; without this the coord side of that story is invisible
  | "cell.seq_gap"              // SPA saw a cell-frame seq discontinuity (frame lost in transit) → forced a catch-up claim. A BURST means the socket is losing frames, not that recovery is broken
  | "cell.foreground_stall"     // foreground terminal liveness bound fired; kv.layer=view_ack|terminal_proof|dom_reconcile and kv.action=resync|redial|reconcile name the failed proof and recovery
  | "cell.paint_lag"            // PTY→browser-arrival latency for a cell frame exceeded the per-session felt-lag floor by PAINT_LAG_SIGNAL_MS (skew-corrected); kv's per-hop values name the hop that owns the delay
  | "event.append_failed"       // coord appendEvent DB tx failed (event-log durability)
  | "audit.write_failed"        // coord audit_log insert failed (audit/compliance trail hole)
  | "audit.input_queue_backpressure" // coord terminal-input audit queue reached its bounded capacity; producers are waiting for durable audit writes
  | "worker.auth_rejected"      // coord rejected a worker WS upgrade (jwt invalid / fp mismatch); kv.reason
  | "worker.protocol_violation" // worker sent event-before-hello / an undecodable frame; kv.reason
  | "worker.queue_overflow"     // coord bounded worker-frame queue rejected a frame; socket closes with 1009 before retaining it
  | "worker.event_rate_exceeded" // coord worker durable-event window exceeded 600/minute; socket closes before persistence
  | "rpc.worker_timeout"        // coord→worker pending RPC timed out; browser spawn/attach hangs
  | "auth.rpc_rejected"         // a Connect RPC returned 401/Unauthenticated (jwt verify fail or no caller); kv.reason,path
  | "worker.uncaught"           // worker uncaughtException/unhandledRejection (mirror of spa.uncaught); kv.kind=error|rejection
  | "transport.event_drop"      // SessionEvent evicted from the unacked outbox on overflow (at-least-once broken = data loss)
  | "transport.raw_metadata_drop" // bounded worker/CoordLink metadata lane rejected bytes; cells remain authoritative but the title/activity scanners saw a gap
  | "transport.metadata_coalesced" // replaceable cwd/git/pr/ports event was superseded or rejected within its bounded volatile lane; lifecycle durability is unaffected
  | "transport.snapshot_unready" // worker snapshot exceeded membership/byte limits or could not be encoded; the authenticated worker stays locally unready until a valid exact snapshot can cross the barrier
  | "heartbeat.stalled"         // N consecutive heartbeat failures to coord (worker invisible to fleet)
  | "scrollback.history_lost"   // resume fell back to an empty ring after getHistory failed (full scrollback wipe)
  | "worker.coord_relocate_failed" // worker STAGE/ACTIVATE/COMMIT/ABORT of a coordinator move threw; kv.action,handoff_id
  | "deploy.failed"             // detached `roost deploy <host>` timed out / exited non-zero / failed to spawn
  | "deploy.cert_skipped"       // deploy continued on plain-ws after `tailscale cert` failed (worker without TLS)
  | "auth.jwt_sign_fail";       // SPA/worker failed to sign the coordinator JWT (RPCs go out unauthenticated)

type SignalSink = (record: Record<string, unknown>) => void;
let _signalSink: SignalSink | null = null;
export function setSignalSink(sink: SignalSink | null): void { _signalSink = sink; }

// Per-(kind+key) cooldown so a flapping source can't flood the channel.
const SIGNAL_COOLDOWN_MS = 10_000;
const _signalLastFire = new Map<string, number>();
// Distinct scope keys (per-sid anomalies) would accumulate forever on a
// long-lived coordinator; drop cold entries once the map is clearly
// larger than any realistic flapping anomaly set.
const SIGNAL_COOLDOWN_MAX_KEYS = 512;

/**
 * Emit ONE always-on Tier-1 signal. Cooldown-gated (default 10s) per
 * `kind` + an optional scope key (kv.cooldownKey ?? kv.sid) so repeats of
 * the same anomaly coalesce while distinct sessions stay independent.
 */
export function signal(kind: SignalKind, kv: DiagKv = {}): void {
  const scope = String(kv.cooldownKey ?? kv.sid ?? "");
  const cooldownKey = `${kind}|${scope}`;
  const now = Date.now();
  if (now - (_signalLastFire.get(cooldownKey) ?? 0) < SIGNAL_COOLDOWN_MS) return;
  if (_signalLastFire.size > SIGNAL_COOLDOWN_MAX_KEYS) {
    for (const [key, firedAt] of _signalLastFire) {
      if (now - firedAt >= SIGNAL_COOLDOWN_MS) _signalLastFire.delete(key);
    }
  }
  _signalLastFire.set(cooldownKey, now);
  const record: Record<string, unknown> = { evt: kind, mono_ns: monoNs(), ...kv };
  delete record.cooldownKey;
  if (_signalSink) { _signalSink(record); return; }
  log.warn("signal", kind, record);
}

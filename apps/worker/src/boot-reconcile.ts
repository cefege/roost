// Reconcile + keeper-liveness wiring for worker boot. Extracted from main.ts:
// owns reconcileOpenSessions (resume/respawn coord's open sessions against
// live keeper PTYs) plus the keeper-death and keeper-degraded remediation
// handlers, which share the reconcile in-flight / last-reconcile state.
// main() calls setupReconcile once after startHeartbeat, then awaits the
// returned reconcileOpenSessions("boot").

import { log, signal } from "@roost/shared";
import type { WorkerFp } from "@roost/shared";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import type { CoordClient } from "./coord-client.ts";
import type { SessionEventSink } from "./event-sink.ts";
import type { SessionManager } from "./session-manager.ts";

export function setupReconcile(deps: {
	client: CoordClient;
	workerFp: WorkerFp;
	sessionMgr: SessionManager;
	sink: SessionEventSink;
}): { reconcileOpenSessions: (reason: string) => Promise<void> } {
	const { client, workerFp, sessionMgr, sink } = deps;

	// Resume / respawn sessions coord still believes open. Two paths:
	// (a) Keeper PTY survived (`launchctl kickstart -k` of just the worker
	//     process — Mac stayed on). resume() probes the mux pool's
	//     listChannels and re-attaches callbacks.
	// (b) Keeper PTY is gone (full Mac reboot — keeper was a child of the
	//     previous bun process and died with it). respawn() spawns a fresh
	//     keeper at the same cwd + kind under the SAME session_id, emitting
	//     a `respawned` event so the sidebar row stays in place.
	// What's lost in (b): scrollback, running subprocesses, terminal
	// context, exported env vars. What survives: cwd, kind,
	// workspace assignment, sidebar position, `↑`-history (per-cwd HISTFILE).
	// Reconcile sessions coord still believes open against live keeper PTYs:
	// resume() the survivors, respawn() the dead. Runs at boot AND whenever
	// the keeper dies mid-life (registered via setOnKeeperDeath below) — the
	// two failure modes (worker restart / keeper crash) need the SAME repair.
	// `_reconcileInFlight` serializes overlapping triggers (a keeper that
	// dies during the boot reconcile, or twice in a row).
	let _reconcileInFlight = false;
	// Timestamp of the last reconcile. A reconcile (boot / keeper restart) briefly
	// emits emit_no_session on channels not yet remapped → a transient
	// keeper.degraded. Gate degradation-remediation on this so that transient
	// burst can't trigger a keeper-restart loop.
	let _lastReconcileMs = 0;
	const KEEPER_DEGRADED_REMEDIATION_GRACE_MS = 90_000;
	const reconcileOpenSessions = async (reason: string): Promise<void> => {
		if (_reconcileInFlight) {
			log.info("worker", "reconcile_skipped_inflight", { reason });
			return;
		}
		_reconcileInFlight = true;
		_lastReconcileMs = Date.now();
		try {
			// Bump the channel counter past any channel the surviving keeper still
			// holds BEFORE resuming/spawning, else a new spawn collides with an
			// orphaned keeper channel → "channel_id in use" (visible post-restart).
			await sessionMgr.advanceChannelCounterPastKeeper();
			const res = await client.sessionsList({ workerFp, status: "open" });
			const resumeResults = await Promise.all(
				res.sessions.map(async (r) => ({
					session: r,
					resumed: await sessionMgr.resume({
						sessionId: r.id as never,
						channelId: r.channel as never,
						kind: r.kind as never,
						cwd: r.cwd,
					}),
				})),
			);
			const needRespawn = resumeResults.filter((r) => !r.resumed);
			let respawned = 0;
			let respawnFailed = 0;
			for (const o of needRespawn) {
				const respawnArgs = {
					oldSessionId: o.session.id as never,
					cwd: o.session.cwd,
					kind: "shell" as const,
				};
				let ok = false;
				// Transient keeper readiness failures are retried; a terminal spawn
				// error closes the stale session.
				for (let attempt = 1; attempt <= 3; attempt++) {
					try {
						await sessionMgr.respawn(respawnArgs);
						ok = true;
						break;
					} catch (err) {
						const errStr = String(err);
						const transient =
							/socket closed|not connected|ENOTCONN|not ready|timeout|SpawnErr|keeper/i.test(
								errStr,
							);
						if (attempt < 3 && transient) {
							log.info("worker", "respawn_retry_transient", {
								sessionId: o.session.id,
								attempt,
								error: errStr,
							});
							try {
								await getMultiplexedPool().ensure();
							} catch {
								/* retry will surface it */
							}
							const { promise, resolve } = Promise.withResolvers<void>();
							setTimeout(resolve, attempt * 400);
							await promise;
							continue;
						}
						log.warn("worker", "respawn_failed", {
							sessionId: o.session.id,
							cwd: o.session.cwd,
							error: errStr,
							transient,
							after_retry: attempt > 1,
						});
						// Breadcrumb model: ONLY a terminal cause (cwd ENOENT, binary missing)
						// closes the row — that session genuinely can't reopen. A transient
						// failure that outlived the retries (keeper still settling after a Mac
						// reboot) leaves the row as an offline breadcrumb — NOT tombstoned (that
						// was the "rows vanish after a Mac restart" bug) — so the sidebar keeps
						// your place and a later reconcile can respawn it.
						if (!transient) {
							await sink.emit({
								kind: "closed",
								ts: Date.now(),
								session_id: o.session.id as never,
								exit_code: null,
							});
						}
						respawnFailed++;
						break;
					}
				}
				if (ok) respawned++;
			}
			// Reverse-reap in the same pass: after adopting/respawning coord's open
			// set, sweep keeper PTYs the worker doesn't track (ghost from a deleted
			// session whose KillChild no-op'd, or a prior keeper generation). A
			// keeper-death/boot reconcile is exactly when strays surface, so seed the
			// sweep here instead of waiting for the first STRAY_REAP_INTERVAL_MS tick
			// (two-strike means the kill lands on the next observation, ≤ one
			// interval). Subsumes advanceChannelCounterPastKeeper's collision concern
			// — the orphan channels it dodged now get reaped.
			const strays = await sessionMgr.reapStrayKeeperChannels();
			log.info("worker", "resume_attempted", {
				reason,
				candidates: res.sessions.length,
				resumed: resumeResults.filter((r) => r.resumed).length,
				respawned,
				respawn_failed: respawnFailed,
				strays_reaped: strays,
			});
		} catch (e) {
			log.warn("worker", "resume_failed", { reason, error: String(e) });
		} finally {
			_reconcileInFlight = false;
		}
	};

	// Mid-life keeper death (the keeper subprocess crashed/was-killed while
	// this worker stayed up) drives the SAME reconcile. Without this, the
	// loop only ran at boot and a keeper death orphaned every PTY as a
	// 'not connected' zombie until a manual worker restart.
	getMultiplexedPool().setOnKeeperDeath(() => {
		log.warn("worker", "keeper_death_reconcile", {});
		void reconcileOpenSessions("keeper_death");
	});

	// Self-heal a DEGRADED survivor keeper (births dead PTYs → emit_no_session
	// bursts → "can't input"). On sustained degradation, force a fresh keeper —
	// but ONLY outside the post-reconcile grace window, else the transient burst
	// a reconcile itself causes would loop. The restart triggers keeper_death →
	// reconcile, which bumps _lastReconcileMs and suppresses re-fire for the
	// grace window. See project_keeper_death_auto_respawn.
	// Restart budget: each restartKeeper SIGTERMs every live PTY, so looping
	// restarts just flaps the user's sessions. The 90s grace only DELAYS the next
	// restart — it has no terminal give-up state. After KEEPER_RESTART_BUDGET
	// restarts in the window, declare the keeper unrecoverable, escalate a Tier-1
	// signal, and STOP (CLAUDE.md keeper-degradation memory — restart-loop fix).
	const _keeperRestarts: number[] = [];
	const KEEPER_RESTART_BUDGET = 2;
	const KEEPER_RESTART_BUDGET_WINDOW_MS = 5 * 60_000;
	sessionMgr.setOnKeeperDegraded(() => {
		const sinceReconcile = Date.now() - _lastReconcileMs;
		if (sinceReconcile < KEEPER_DEGRADED_REMEDIATION_GRACE_MS) {
			log.info("worker", "keeper_degraded_skip_transient", {
				since_reconcile_ms: sinceReconcile,
			});
			return;
		}
		const now = Date.now();
		const windowStart = now - KEEPER_RESTART_BUDGET_WINDOW_MS;
		while (_keeperRestarts.length && _keeperRestarts[0]! < windowStart)
			_keeperRestarts.shift();
		if (_keeperRestarts.length >= KEEPER_RESTART_BUDGET) {
			signal("keeper.degraded_unrecoverable", {
				restarts: _keeperRestarts.length,
				window_ms: KEEPER_RESTART_BUDGET_WINDOW_MS,
				cooldownKey: "keeper",
			});
			log.error("worker", "keeper_degraded_unrecoverable", {
				restarts: _keeperRestarts.length,
			});
			return;
		}
		_keeperRestarts.push(now);
		log.warn("worker", "keeper_degraded_restart", {
			since_reconcile_ms: sinceReconcile,
			restart_n: _keeperRestarts.length,
		});
		getMultiplexedPool().restartKeeper();
	});

	return { reconcileOpenSessions };
}

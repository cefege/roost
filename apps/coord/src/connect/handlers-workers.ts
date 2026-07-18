// Worker-registry RPC handlers: list/register/heartbeat/rename/delete, the
// deploy-job start + output stream, and the (not-yet-implemented) transfer
// stubs. Registration/heartbeat fan worker presence out on presenceBus.
// Spread into router.ts's single router.service() literal. Split out of
// router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
	type CoordinatorService,
	WorkersListResponseSchema,
	WorkersRegisterResponseSchema,
	WorkersHeartbeatResponseSchema,
	WorkersRenameResponseSchema,
	WorkersDeleteResponseSchema,
	WorkersDeployStartResponseSchema,
	WorkersDeployOutputFrameSchema,
	TransfersStartResponseSchema,
	TransfersOutputFrameSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
	workerRowToProto,
	workerRowToWirePresence,
} from "@roost/shared/wire/row-proto";
import { presenceBus } from "../buses.ts";
import { startDeploy, deployOutput } from "../deploy-jobs.ts";
import { listRoutableFps } from "./worker-service.ts";
import { requireAuth } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type WorkerMethods =
	| "workersList"
	| "workersRegister"
	| "workersHeartbeat"
	| "workersRename"
	| "workersDelete"
	| "workersDeployOutput"
	| "transfersStart"
	| "transfersOutput"
	| "workersDeployStart";

export function makeWorkerHandlers(
	deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, WorkerMethods> {
	return {
		async workersList(_req, ctx) {
			requireAuth(ctx.values);
			const rows = await deps.db.selectFrom("workers").selectAll().execute();
			return create(WorkersListResponseSchema, {
				workers: rows.map(workerRowToProto),
				routableFps: listRoutableFps(),
			});
		},

		async workersRegister(req, ctx) {
			const caller = requireAuth(ctx.values);
			const fp = caller.fingerprint;
			const existing = await deps.db
				.selectFrom("workers")
				.selectAll()
				.where("fp", "=", fp)
				.executeTakeFirst();
			if (!existing)
				throw new ConnectError(
					"worker not registered; redeem bootstrap token first",
					Code.Unauthenticated,
				);
			const now = Date.now();
			const updated = await deps.db
				.updateTable("workers")
				.set({
					label: req.label ?? existing.label,
					os: req.os ?? existing.os,
					git_sha: req.gitSha ?? existing.git_sha,
					reachable_addr: req.reachableAddr ?? existing.reachable_addr,
					last_seen_ms: now,
				})
				.where("fp", "=", fp)
				.returningAll()
				.executeTakeFirstOrThrow();
			const w = workerRowToProto(updated);
			presenceBus.publish({
				kind: "registered",
				worker: workerRowToWirePresence(updated) as any,
			});
			return create(WorkersRegisterResponseSchema, { worker: w });
		},

		async workersHeartbeat(req, ctx) {
			const caller = requireAuth(ctx.values);
			const fp = caller.fingerprint;
			const now = Date.now();
			const prior = await deps.db
				.selectFrom("workers")
				.select(["git_sha", "keeper_stale", "reachable_addr"])
				.where("fp", "=", fp)
				.executeTakeFirst();
			const hm = req.hostMetrics
				? {
						cpu_pct: req.hostMetrics.cpuPct,
						mem_used_bytes: Number(req.hostMetrics.memUsedBytes),
						mem_total_bytes: Number(req.hostMetrics.memTotalBytes),
						disk_used_bytes: Number(req.hostMetrics.diskUsedBytes),
						disk_total_bytes: Number(req.hostMetrics.diskTotalBytes),
						net_rx_bps: Number(req.hostMetrics.netRxBps),
						net_tx_bps: Number(req.hostMetrics.netTxBps),
						sampled_at_ms: Number(req.hostMetrics.sampledAtMs),
					}
				: undefined;
			// keeper_stale: worker sends "" when the keeper is current, the running
			// stamp when stale, undefined if it hasn't probed yet. Map ""→null.
			const newKeeperStale =
				req.keeperStale !== undefined
					? req.keeperStale || null
					: (prior?.keeper_stale ?? null);
			// reachable_addr self-heals on every beat: the worker re-resolves its
			// LIVE tailnet DNSName each beat (heartbeat.ts) so a machine rename
			// corrects within 30s, not only at boot. Only persist a non-empty value
			// — an absent/empty field (tailscale unreachable this beat) keeps the
			// prior value rather than nulling a good address.
			const newReachableAddr =
				req.reachableAddr && req.reachableAddr.length > 0
					? req.reachableAddr
					: undefined;
			await deps.db
				.updateTable("workers")
				.set({
					last_seen_ms: now,
					...(req.gitSha !== undefined && { git_sha: req.gitSha }),
					...(req.keeperStale !== undefined && {
						keeper_stale: req.keeperStale || null,
					}),
					...(hm !== undefined && { host_metrics_json: JSON.stringify(hm) }),
					...(newReachableAddr !== undefined && {
						reachable_addr: newReachableAddr,
					}),
				})
				.where("fp", "=", fp)
				.execute();
			const gitShaChanged =
				req.gitSha !== undefined && prior?.git_sha !== req.gitSha;
			const keeperStaleChanged =
				(prior?.keeper_stale ?? null) !== newKeeperStale;
			const reachableChanged =
				newReachableAddr !== undefined &&
				prior?.reachable_addr !== newReachableAddr;
			if (gitShaChanged || keeperStaleChanged || reachableChanged) {
				const row = await deps.db
					.selectFrom("workers")
					.selectAll()
					.where("fp", "=", fp)
					.executeTakeFirstOrThrow();
				presenceBus.publish({
					kind: "registered",
					worker: workerRowToWirePresence(row) as any,
				});
			} else {
				presenceBus.publish({
					kind: "heartbeat",
					fp: fp as any,
					last_seen_ms: now,
					host_metrics: hm ?? null,
				});
			}
			return create(WorkersHeartbeatResponseSchema, {
				coordPubkeyB64: deps.coordKey.verifyingKeyB64(),
				coordPubkeyKid: deps.coordKey.verifyingKeyKid(),
			});
		},

		async workersRename(req, ctx) {
			requireAuth(ctx.values);
			const existing = await deps.db
				.selectFrom("workers")
				.selectAll()
				.where("fp", "=", req.fp)
				.executeTakeFirst();
			if (!existing) throw new ConnectError("worker not found", Code.NotFound);
			const updated = await deps.db
				.updateTable("workers")
				.set({ label: req.label })
				.where("fp", "=", req.fp)
				.returningAll()
				.executeTakeFirstOrThrow();
			presenceBus.publish({
				kind: "registered",
				worker: workerRowToWirePresence(updated) as any,
			});
			return create(WorkersRenameResponseSchema, {
				worker: workerRowToProto(updated),
			});
		},

		async workersDelete(req, ctx) {
			requireAuth(ctx.values);
			// Atomic: if the workers delete fails (FK violation from a row
			// in sessions referencing this worker), authorized_keys is
			// rolled back too. Prior shape ran them as separate statements,
			// so on FK failure the worker was already un-auth'd (next
			// heartbeat 401) but the row stayed alive in the sidebar.
			await deps.db.transaction().execute(async (trx) => {
				await trx.deleteFrom("workers").where("fp", "=", req.fp).execute();
				await trx
					.deleteFrom("authorized_keys")
					.where("fingerprint", "=", req.fp)
					.execute();
			});
			presenceBus.publish({ kind: "removed", fp: req.fp as any });
			return create(WorkersDeleteResponseSchema, { ok: true });
		},

		async *workersDeployOutput(req, ctx) {
			requireAuth(ctx.values);
			for await (const msg of deployOutput(req.jobId, ctx.signal)) {
				if (msg.kind === "line") {
					yield create(WorkersDeployOutputFrameSchema, {
						kind: "line",
						text: msg.text,
					});
				} else {
					yield create(WorkersDeployOutputFrameSchema, {
						kind: "done",
						exit: msg.exit ?? -1,
						error: msg.error ?? "",
					});
				}
			}
		},

		async transfersStart(req, _ctx) {
			requireAuth(_ctx.values);
			// transfers.start is currently unimplemented server-side; return a
			// sentinel error so the SPA's TransferDialog can surface it.
			return create(TransfersStartResponseSchema, {
				ok: false,
				jobId: "",
				error: "transfers not yet implemented",
			});
		},

		async *transfersOutput(_req, _ctx) {
			requireAuth(_ctx.values);
			// No live transfer jobs; immediately emit a done frame.
			yield create(TransfersOutputFrameSchema, {
				kind: "done",
				exit: 0,
				error: "transfers not yet implemented",
			});
		},

		async workersDeployStart(req, ctx) {
			requireAuth(ctx.values);
			const result = startDeploy(req.host);
			return create(WorkersDeployStartResponseSchema, {
				ok: result.ok,
				jobId: result.jobId ?? "",
				error: result.error ?? "",
			});
		},
	};
}

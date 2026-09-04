// Owns worker registry RPCs and publishes every persisted presence transition
// after its database write. Deletion fences the authoritative connection
// immediately after commit, then isolates each volatile cleanup so one failed
// projection cannot leave the remaining worker state live.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { isSupportedHostPlatform } from "@roost/shared/platform";
import {
	type CoordinatorService,
	WorkersListResponseSchema,
	WorkersRegisterResponseSchema,
	WorkersHeartbeatResponseSchema,
	WorkersRenameResponseSchema,
	WorkersDeleteResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
	workerRowToProto,
	workerRowToWirePresence,
} from "@roost/shared/wire/row-proto";
import { presenceBus } from "../buses.ts";
import { listRoutableFps } from "./worker-service.ts";
import {
	requireDashboardActor,
	requireDashboardAdmin,
	requireWorker,
} from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import { invalidateJwtKey } from "../jwt.ts";
import { truncatePersistedUtf8 } from "../persistence-input.ts";
import { asWorkerFp } from "@roost/shared/wire";
import { retireWorkerRoutes } from "../byte-hub.ts";
import {
	fenceWorkerCredential,
	_publishRoutable,
} from "./worker-registry.ts";
import { notifyTerminalWorkerRetired } from "./terminal-view-hub.ts";
import { log } from "@roost/shared/log";
import { makeWorkerDeployHandlers } from "./handlers-workers-deploy.ts";
export {
	resolveWorkerDeployTarget,
	workerDeployHost,
	type WorkerDeployRecord,
	type WorkerDeployTargetResolution,
} from "./handlers-workers-deploy.ts";

function bestEffortWorkerDeleteCleanup(
	step: string,
	work: () => void,
): void {
	try {
		work();
	} catch (error) {
		log.warn("workers-delete", "cleanup_failed", {
			step,
			error: String(error),
		});
	}
}

type WorkerMethods =
	| "workersList"
	| "workersRegister"
	| "workersHeartbeat"
	| "workersRename"
	| "workersDelete"
	| "workersDeployOutput"
	| "workersDeployStart";

export function makeWorkerHandlers(
	deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, WorkerMethods> {

	return {
		async workersList(_req, ctx) {
			const actor = requireDashboardActor(ctx.values);
			const rows = await deps.db
				.selectFrom("workers")
				.selectAll()
				.where("dashboard_id", "=", actor.dashboardId)
				.where("deleted_at_ms", "is", null)
				.execute();
			const workerFps = new Set(rows.map((worker) => worker.fp));
			return create(WorkersListResponseSchema, {
				workers: rows.map(workerRowToProto),
				routableFps: listRoutableFps().filter((fp) => workerFps.has(fp)),
			});
		},

		async workersRegister(req, ctx) {
			if (req.os !== undefined && !isSupportedHostPlatform(req.os)) {
				throw new ConnectError("unsupported worker os", Code.InvalidArgument);
			}
			const caller = requireWorker(ctx.values);
			const fp = caller.fingerprint;
			const label = req.label === undefined
				? undefined
				: truncatePersistedUtf8(req.label);
			const gitSha = req.gitSha === undefined
				? undefined
				: truncatePersistedUtf8(req.gitSha);
			const reachableAddr = req.reachableAddr === undefined
				? undefined
				: truncatePersistedUtf8(req.reachableAddr);
			const existing = await deps.db
				.selectFrom("workers")
				.selectAll()
				.where("fp", "=", fp)
				.where("dashboard_id", "=", caller.dashboardId)
				.where("deleted_at_ms", "is", null)
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
					label: label ?? existing.label,
					os: req.os ?? existing.os,
					git_sha: gitSha ?? existing.git_sha,
					reachable_addr: reachableAddr ?? existing.reachable_addr,
					last_seen_ms: now,
				})
				.where("fp", "=", fp)
				.where("dashboard_id", "=", caller.dashboardId)
				.where("deleted_at_ms", "is", null)
				.returningAll()
				.executeTakeFirstOrThrow();
			const w = workerRowToProto(updated);
			presenceBus.publish({
				kind: "registered",
				worker: workerRowToWirePresence(updated) as any,
				_dashboard_id: caller.dashboardId,
			});
			return create(WorkersRegisterResponseSchema, { worker: w });
		},

		async workersHeartbeat(req, ctx) {
			const caller = requireWorker(ctx.values);
			const fp = caller.fingerprint;
			const newKeeperStale = req.keeperStale === undefined
				? null
				: truncatePersistedUtf8(req.keeperStale);
			const newGitSha = req.gitSha === undefined
				? undefined
				: truncatePersistedUtf8(req.gitSha);
			const newReachableAddr =
				req.reachableAddr && req.reachableAddr.length > 0
					? truncatePersistedUtf8(req.reachableAddr)
					: undefined;
			const now = Date.now();
			const prior = await deps.db
				.selectFrom("workers")
				.select(["git_sha", "keeper_stale", "reachable_addr", "dashboard_id"])
				.where("fp", "=", fp)
				.where("dashboard_id", "=", caller.dashboardId)
				.where("deleted_at_ms", "is", null)
				.executeTakeFirst();
			if (!prior)
				throw new ConnectError(
					"worker not registered; redeem bootstrap token first",
					Code.Unauthenticated,
				);
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
			// Preserve all three states in the existing nullable column:
			// null = unknown/unreported, "" = current, non-empty = stale build.
			// reachable_addr self-heals on every beat: the worker re-resolves its
			// LIVE tailnet DNSName each beat (heartbeat.ts) so a machine rename
			// corrects within 30s, not only at boot. Only persist a non-empty value
			// — an absent/empty field (tailscale unreachable this beat) keeps the
			// prior value rather than nulling a good address.
			const updated = await deps.db
				.updateTable("workers")
				.set({
					last_seen_ms: now,
					...(newGitSha !== undefined && { git_sha: newGitSha }),
					keeper_stale: newKeeperStale,
					...(hm !== undefined && { host_metrics_json: JSON.stringify(hm) }),
					...(newReachableAddr !== undefined && {
						reachable_addr: newReachableAddr,
					}),
				})
				.where("fp", "=", fp)
				.where("dashboard_id", "=", caller.dashboardId)
				.where("deleted_at_ms", "is", null)
				.returningAll()
				.executeTakeFirst();
			if (!updated)
				throw new ConnectError(
					"worker not registered; redeem bootstrap token first",
					Code.Unauthenticated,
				);
			const gitShaChanged =
				newGitSha !== undefined && prior?.git_sha !== newGitSha;
			const keeperStaleChanged =
				(prior?.keeper_stale ?? null) !== newKeeperStale;
			const reachableChanged =
				newReachableAddr !== undefined &&
				prior?.reachable_addr !== newReachableAddr;
			if (gitShaChanged || keeperStaleChanged || reachableChanged) {
				presenceBus.publish({
					kind: "registered",
					worker: workerRowToWirePresence(updated) as any,
					_dashboard_id: caller.dashboardId,
				});
			} else {
				presenceBus.publish({
					kind: "heartbeat",
					fp: fp as any,
					last_seen_ms: now,
					host_metrics: hm ?? null,
					_dashboard_id: caller.dashboardId,
				});
			}
			return create(WorkersHeartbeatResponseSchema, {});
		},

		async workersRename(req, ctx) {
			const actor = requireDashboardAdmin(ctx.values);
			const label = truncatePersistedUtf8(req.label);
			const existing = await deps.db
				.selectFrom("workers")
				.selectAll()
				.where("fp", "=", req.fp)
				.where("dashboard_id", "=", actor.dashboardId)
				.where("deleted_at_ms", "is", null)
				.executeTakeFirst();
			if (!existing) throw new ConnectError("worker not found", Code.NotFound);
			const updated = await deps.db
				.updateTable("workers")
				.set({ label })
				.where("fp", "=", req.fp)
				.where("dashboard_id", "=", actor.dashboardId)
				.where("deleted_at_ms", "is", null)
				.returningAll()
				.executeTakeFirstOrThrow();
			presenceBus.publish({
				kind: "registered",
				worker: workerRowToWirePresence(updated) as any,
				_dashboard_id: actor.dashboardId,
			});
			return create(WorkersRenameResponseSchema, {
				worker: workerRowToProto(updated),
			});
		},

		async workersDelete(req, ctx) {
			const actor = requireDashboardAdmin(ctx.values);
			const now = Date.now();
			const persistedSessionIds = await deps.db.transaction().execute(async (trx) => {
				const worker = await trx
					.selectFrom("workers")
					.select("fp")
					.where("fp", "=", req.fp)
					.where("dashboard_id", "=", actor.dashboardId)
					.where("deleted_at_ms", "is", null)
					.executeTakeFirst();
				if (!worker) throw new ConnectError("worker not found", Code.NotFound);
				const sessionRows = await trx.selectFrom("sessions")
					.select("id")
					.where("worker_fp", "=", req.fp)
					.where("dashboard_id", "=", actor.dashboardId)
					.execute();
				await trx.insertInto("authorized_key_revocations").values({
					fingerprint: req.fp,
					revoked_at_ms: now,
					revoked_by_fp: actor.deviceFingerprint,
					reason: "worker-deleted",
				}).execute();
				const tombstone = await trx.updateTable("workers")
					.set({ deleted_at_ms: now })
					.where("fp", "=", req.fp)
					.where("dashboard_id", "=", actor.dashboardId)
					.where("deleted_at_ms", "is", null)
					.returning("fp")
					.executeTakeFirst();
				if (!tombstone) throw new Error("worker tombstone update lost");
				await trx.deleteFrom("bootstrap_tokens")
					.where("used_at_ms", "is", null)
					.where("dashboard_id", "=", actor.dashboardId)
					.where("minted_by_fp", "=", req.fp)
					.execute();
				await trx.deleteFrom("authorized_keys")
					.where("fingerprint", "=", req.fp)
					.execute();
				return sessionRows.map((row) => row.id);
			});

			// The commit is irrevocable. Fence synchronously before any
			// best-effort cleanup can yield, publish, or fail.
			try {
				deps.onWorkerDeletedFence?.(req.fp);
			} catch (error) {
				log.warn("workers-delete", "fence_callback_failed", {
					worker_fp: req.fp,
					error: String(error),
				});
			} finally {
				// Always fence the process-global authoritative handle, including
				// test/portable runtimes without a Bun WebSocket owner.
				fenceWorkerCredential(req.fp);
				deps.pendingPublications?.clearWorker(req.fp);
			}

			let retiredSessionIds = persistedSessionIds;
			bestEffortWorkerDeleteCleanup("jwt_cache", () => {
				invalidateJwtKey(deps.jwtCache, req.fp);
			});
			bestEffortWorkerDeleteCleanup("routes", () => {
				const volatileIds = retireWorkerRoutes(asWorkerFp(req.fp));
				retiredSessionIds = [...new Set([
					...persistedSessionIds,
					...volatileIds,
				])];
			});
			bestEffortWorkerDeleteCleanup("terminal_views", () => {
				notifyTerminalWorkerRetired(req.fp, retiredSessionIds);
			});
			bestEffortWorkerDeleteCleanup("routable_presence", _publishRoutable);
			bestEffortWorkerDeleteCleanup("sync_scope", () => {
				deps.onWorkerDeletedSyncScope?.(actor.dashboardId, req.fp);
			});
			bestEffortWorkerDeleteCleanup("worker_presence", () => {
				presenceBus.publish({
					kind: "removed",
					fp: asWorkerFp(req.fp),
					_dashboard_id: actor.dashboardId,
				});
			});
			bestEffortWorkerDeleteCleanup("socket_close", () => {
				deps.onWorkerDeletedSocketClose?.(req.fp);
			});
			return create(WorkersDeleteResponseSchema, { ok: true });
		},

		...makeWorkerDeployHandlers(deps),
	};
}

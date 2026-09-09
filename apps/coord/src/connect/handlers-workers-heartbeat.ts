// Owns the worker heartbeat RPC: the per-beat liveness write and the presence
// frame it publishes. Split from handlers-workers.ts, which spreads this
// handler into the single router.service() literal.
// Every field this beat re-asserts (git_sha, reachable_addr, os) self-heals a
// row that would otherwise keep an enrollment-time value forever.

import { create } from "@bufbuild/protobuf";
import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { log } from "@roost/shared/log";
import { isSupportedHostPlatform } from "@roost/shared/platform";
import { keeperRuntimeObservationFromProto } from "@roost/shared/keeper-update-proto";
import {
	type CoordinatorService,
	WorkersHeartbeatResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { workerRowToWirePresence } from "@roost/shared/wire/row-proto";
import { asWorkerFp, type Worker as WireWorker } from "@roost/shared/wire";
import { presenceBus } from "../buses.ts";
import { truncatePersistedUtf8 } from "../persistence-input.ts";
import { requireWorker } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

export function makeWorkerHeartbeatHandler(
	deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, "workersHeartbeat"> {
	return {
		async workersHeartbeat(req, ctx) {
			if (req.os !== undefined && !isSupportedHostPlatform(req.os)) {
				throw new ConnectError("unsupported worker os", Code.InvalidArgument);
			}
			const caller = requireWorker(ctx.values);
			const fp = caller.fingerprint;
			let newKeeperRuntimeJson: string | null = null;
			let keeperRuntimeMalformed = false;
			if (req.keeperRuntime) {
				try {
					newKeeperRuntimeJson = JSON.stringify(
						keeperRuntimeObservationFromProto(req.keeperRuntime),
					);
				} catch {
					keeperRuntimeMalformed = true;
				}
			}
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
				.select([
					"os",
					"git_sha",
					"keeper_runtime_json",
					"reachable_addr",
				])
				.where("fp", "=", fp)
				.where("deleted_at_ms", "is", null)
				.executeTakeFirst();
			if (!prior)
				throw new ConnectError(
					"worker not registered; redeem bootstrap token first",
					Code.Unauthenticated,
				);
			if (keeperRuntimeMalformed) {
				const cleared = await deps.db
					.updateTable("workers")
					.set({
						last_seen_ms: now,
						keeper_runtime_json: null,
					})
					.where("fp", "=", fp)
					.where("deleted_at_ms", "is", null)
					.returningAll()
					.executeTakeFirstOrThrow();
				presenceBus.publish({
					kind: "registered",
					worker: workerRowToWirePresence(cleared) as unknown as WireWorker,
				});
				throw new ConnectError(
					"keeper runtime observation is malformed",
					Code.InvalidArgument,
				);
			}
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
			// reachable_addr self-heals on every beat: the worker re-resolves its
			// LIVE address each beat (heartbeat.ts) so a machine rename corrects
			// within 30s, not only at boot. Only persist a non-empty value — an
			// absent/empty field (resolution failed this beat) keeps the prior
			// value rather than nulling a good address.
			// os self-heals for the same reason, and the invariant is stronger: the
			// stored platform must describe the process currently beating on this
			// fingerprint. It is otherwise write-once at enrollment, so a row could
			// name a machine that no longer holds the key. A flip means two machines
			// share one worker key, which is invisible without this log line.
			const osChanged = req.os !== undefined && req.os !== prior.os;
			if (osChanged) {
				log.warn("workers", "worker_os_changed", {
					fp,
					from: prior.os,
					to: req.os,
				});
			}
			const updated = await deps.db
				.updateTable("workers")
				.set({
					last_seen_ms: now,
					...(newGitSha !== undefined && { git_sha: newGitSha }),
					keeper_runtime_json: newKeeperRuntimeJson,
					...(hm !== undefined && { host_metrics_json: JSON.stringify(hm) }),
					...(newReachableAddr !== undefined && {
						reachable_addr: newReachableAddr,
					}),
					...(req.os !== undefined && { os: req.os }),
				})
				.where("fp", "=", fp)
				.where("deleted_at_ms", "is", null)
				.returningAll()
				.executeTakeFirst();
			if (!updated)
				throw new ConnectError(
					"worker not registered; redeem bootstrap token first",
					Code.Unauthenticated,
				);
			const gitShaChanged =
				newGitSha !== undefined && prior.git_sha !== newGitSha;
			const keeperRuntimeChanged =
				(prior.keeper_runtime_json ?? null) !== newKeeperRuntimeJson;
			const reachableChanged =
				newReachableAddr !== undefined &&
				prior.reachable_addr !== newReachableAddr;
			// A platform flip must ride the full row frame, not the light heartbeat
			// delta: the SPA's worker record keeps its stale os until a registered
			// frame replaces it.
			if (
				gitShaChanged
				|| keeperRuntimeChanged
				|| reachableChanged
				|| osChanged
			) {
				presenceBus.publish({
					kind: "registered",
					worker: workerRowToWirePresence(updated) as unknown as WireWorker,
				});
			} else {
				presenceBus.publish({
					kind: "heartbeat",
					fp: asWorkerFp(fp),
					last_seen_ms: now,
					host_metrics: hm ?? null,
				});
			}
			return create(WorkersHeartbeatResponseSchema, {});
		},
	};
}

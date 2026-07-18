// Keeper survivor pre-clean for worker boot. Extracted from main.ts:
// probe a leftover keeper subprocess/UDS from a previous worker generation
// and either adopt the compatible survivor or kill the incompatible one.

import { log } from "@roost/shared";
import {
	probeKeeperCompatible,
	getMultiplexedPool,
} from "./keeper/multiplexed-client.ts";
import { KEEPER_BUILD_STAMP } from "./keeper/keeper-stamp.ts";
import { existsSync, unlinkSync } from "node:fs";

// Pre-clean any keeper subprocess + UDS file left over from a previous
// worker generation. `launchctl kickstart -k com.roost.worker-v2` SIGKILLs
// the worker, but the multiplexed-main.ts keeper was spawned `detached:
// true` (see multiplexed-client.ts) and survives. Whether to kill the
// survivor depends on protocol compatibility:
//
//   compatible (Hello/HelloResp version matches KEEPER_PROTOCOL_VERSION):
//     PRESERVE the keeper. resume() probes its live channel set via
//     ListChannels + reattaches per-channel callbacks → every running
//     shell/claude session survives the worker restart with scrollback
//     intact.
//
//   incompatible (no Hello response within 800ms OR version mismatch):
//     KILL it. Old keepers either pre-date Hello (silently drop it) or
//     reply with a stale version after a protocol-bump deploy. Their
//     in-memory protocol code can hang Spawn frames for 15s ("Create
//     folder failed: [internal] internal error" in the SPA) so a clean
//     slate beats a torn one.
//
// Brand-new install with no sock file → probe returns true immediately,
// kill is no-op, ensure() spawns a fresh keeper.
function killStaleKeeper(sockPath: string): void {
	try {
		const r = Bun.spawnSync(
			["pgrep", "-f", `multiplexed-main.ts ${sockPath}`],
			{ timeout: 2000, killSignal: "SIGKILL" },
		);
		if (r.exitCode === 0) {
			const pids = r.stdout.toString().trim().split("\n").filter(Boolean);
			for (const pid of pids) {
				try {
					process.kill(Number(pid), "SIGTERM");
				} catch {
					/* gone already */
				}
			}
			if (pids.length > 0)
				log.info("worker", "killed_stale_keeper", { pids, sockPath });
		}
	} catch {
		/* pgrep not available — fall through to unlink */
	}
	if (existsSync(sockPath)) {
		try {
			unlinkSync(sockPath);
			log.info("worker", "unlinked_stale_keeper_sock", { sockPath });
		} catch (e) {
			log.warn("worker", "stale_keeper_sock_unlink_failed", {
				error: String(e),
			});
		}
	}
}

export async function handleKeeperSurvivor(sockPath: string): Promise<void> {
	const _probe = await probeKeeperCompatible(sockPath);
	if (_probe.compatible) {
		// Adopt the survivor. Its code stamp may be OLDER than ours (a behavior-only
		// change landed since it spawned) — record it so the heartbeat flags a stale
		// keeper (surfaced in the SPA; apply with `roost keeper-refresh`).
		if (_probe.keeperStamp !== undefined)
			getMultiplexedPool().setRunningKeeperStamp(_probe.keeperStamp);
		log.info("worker", "keeper_survivor_compatible", {
			sockPath,
			keeper_build: _probe.keeperStamp ?? null,
			worker_build: KEEPER_BUILD_STAMP,
			keeper_stale:
				_probe.keeperStamp !== undefined &&
				_probe.keeperStamp !== KEEPER_BUILD_STAMP,
		});
	} else {
		log.info("worker", "keeper_survivor_incompatible_killing", {
			sockPath,
			worker_build: KEEPER_BUILD_STAMP,
		});
		killStaleKeeper(sockPath);
	}
}

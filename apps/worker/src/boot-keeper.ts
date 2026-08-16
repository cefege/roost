// Keeper survivor pre-clean for worker boot. Compatible authenticated
// survivors are adopted; stale keepers are retired without treating a Windows
// named pipe as a filesystem entry.

import {
	cleanupLocalEndpoint,
	log,
	type LocalEndpoint,
} from "@roost/shared";
import {
	getMultiplexedPool,
	probeKeeperCompatible,
} from "./keeper/multiplexed-client.ts";
import {
	shutdownKeeperAuthenticated,
} from "./keeper/keeper-probe.ts";
import { muxLocalEndpoint } from "./keeper/keeper-pool-config.ts";
import { KEEPER_BUILD_STAMP } from "./keeper/keeper-stamp.ts";

/** Legacy drain for a pre-auth POSIX keeper. Windows must never shell out to
 * pgrep or unlink a pipe name; capability-aware keepers use Shutdown instead. */
async function killLegacyPosixKeeper(endpoint: LocalEndpoint): Promise<void> {
	if (endpoint.kind !== "uds") {
		throw new Error("legacy keeper cannot be retired on a non-UDS endpoint");
	}
	try {
		const result = Bun.spawnSync(
			["pgrep", "-f", `multiplexed-main.ts ${endpoint.address}`],
			{ timeout: 2000, killSignal: "SIGKILL" },
		);
		if (result.exitCode === 0) {
			const pids = result.stdout.toString().trim().split("\n").filter(Boolean);
			for (const pid of pids) {
				try {
					process.kill(Number(pid), "SIGTERM");
				} catch {
					/* gone already */
				}
			}
			if (pids.length > 0) {
				log.info("worker", "killed_stale_keeper", {
					pids,
					endpoint: endpoint.address,
				});
			}
		}
	} catch {
		/* pgrep unavailable; removing the UDS preserves the prior drain path */
	}
	await cleanupLocalEndpoint(endpoint);
}

async function waitForKeeperExit(endpoint: LocalEndpoint): Promise<boolean> {
	const deadline = Date.now() + 2_000;
	do {
		const probe = await probeKeeperCompatible(endpoint, 250);
		if (!probe.reachable) return true;
		await Bun.sleep(25);
	} while (Date.now() < deadline);
	return false;
}

export async function handleKeeperSurvivor(): Promise<void> {
	const endpoint = muxLocalEndpoint();
	const probe = await probeKeeperCompatible(endpoint);
	if (!probe.reachable) {
		// Removes only a dead POSIX socket. Named-pipe cleanup is a no-op.
		await cleanupLocalEndpoint(endpoint);
		return;
	}

	if (probe.compatible) {
		if (probe.keeperStamp !== undefined) {
			getMultiplexedPool().setRunningKeeperStamp(probe.keeperStamp);
		}
		log.info("worker", "keeper_survivor_compatible", {
			endpoint: endpoint.address,
			kind: endpoint.kind,
			keeper_build: probe.keeperStamp ?? null,
			worker_build: KEEPER_BUILD_STAMP,
			keeper_stale:
				probe.keeperStamp !== undefined &&
				probe.keeperStamp !== KEEPER_BUILD_STAMP,
		});
		return;
	}

	log.info("worker", "keeper_survivor_incompatible_killing", {
		endpoint: endpoint.address,
		kind: endpoint.kind,
		worker_build: KEEPER_BUILD_STAMP,
		authenticated: probe.authenticated,
	});

	if (probe.authenticated) {
		if (!await shutdownKeeperAuthenticated(endpoint) || !await waitForKeeperExit(endpoint)) {
			throw new Error("authenticated stale keeper did not shut down");
		}
		await cleanupLocalEndpoint(endpoint);
		return;
	}

	if (endpoint.kind === "uds") {
		await killLegacyPosixKeeper(endpoint);
		return;
	}
	throw new Error(
		"named-pipe keeper rejected capability authentication; refusing unsafe replacement",
	);
}

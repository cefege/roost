// Regression: WorkersHeartbeat persists reachable_addr (the worker's LIVE
// tailnet DNSName, re-resolved each beat) so a machine rename self-heals within
// 30s instead of only at boot via workersRegister. Guards the HostName-vs-
// DNSName split-brain bug: worker.label is the Tailscale HostName
// (worker-host) which does NOT resolve; only DNSName (coord-host) does, so
// the SPA composes vnc:// / smb:// from reachable_addr, never the label. Drives
// the real coord.fetch WorkersHeartbeat handler with a worker JWT.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache, signJwt, fingerprintOf } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { presenceBus } from "../src/buses.ts";
import type { CoordConfig } from "@roost/shared/config";

let workdir: string;
let coord: CoordHandle;
let cleanup: () => void;
let workerJwt: string;
let workerFp: string;
let db: KyselyDB;

beforeAll(async () => {
	workdir = mkdtempSync(join(tmpdir(), "roost-hb-reachable-"));
	const dbPath = join(workdir, "test.db");
	const keyPath = join(workdir, "test.key");
	const authPath = join(workdir, "authorized_keys");
	writeFileSync(authPath, "");

	const opened = openDb(dbPath);
	db = opened.db;
	const sqlite = opened.sqlite;
	await runMigrations(sqlite);
	const coordKey = await loadOrCreateCoordKey(keyPath);
	const jwtCache = newJwtCache();
	const cfg: CoordConfig = {
		bind: "127.0.0.1:0",
		dbPath,
		coordKeyPath: keyPath,
		authorizedKeysPath: authPath,
		webDistPath: "",
		tlsCertPath: undefined,
		tlsKeyPath: undefined,
		jwtMaxAgeSecs: 300,
		auditRetentionDays: 90,
		relaxedCsp: false,
		corsAllowedOrigins: [],
		trustedProxyIps: ["127.0.0.1", "::1", "::ffff:127.0.0.1"],
		logDir: workdir,
		publicUrl: undefined,
		handoffPath: join(workdir, "coord-handoff.json"),
	};
	coord = createCoord({ db, sqlite, coordKey, cfg, jwtCache });

	// Mint a worker keypair, authorize it, seed its workers row (register-time
	// reachable_addr intentionally null to prove the heartbeat backfills it).
	const workerKeys = await crypto.subtle.generateKey(
		{ name: "Ed25519" },
		true,
		["sign", "verify"],
	);
	const rawPub = new Uint8Array(
		await crypto.subtle.exportKey("raw", workerKeys.publicKey),
	);
	workerFp = await fingerprintOf(rawPub);
	await db
		.insertInto("authorized_keys")
		.values({
			fingerprint: workerFp,
			public_key: rawPub,
			label: "worker-host", // Tailscale HostName — deliberately unresolvable
			added_at: Date.now(),
		})
		.execute();
	await db
		.insertInto("workers")
		.values({
			fp: workerFp,
			label: "worker-host",
			os: "darwin",
			registered_at_ms: Date.now(),
			last_seen_ms: Date.now(),
			reachable_addr: null,
		})
		.execute();
	const now = Math.floor(Date.now() / 1000);
	workerJwt = await signJwt(
		{ aud: "roost-coordinator", sub: workerFp, iat: now, exp: now + 60 },
		workerKeys.privateKey,
		workerFp,
	);

	cleanup = () => {
		coord.dispose();
		try {
			sqlite.close();
		} catch {
			/* ignore */
		}
		if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
	};
});

afterAll(() => cleanup?.());

function heartbeat(body: unknown): Promise<Response> {
	return coord.fetch(
		new Request("http://t/roost.v1.CoordinatorService/WorkersHeartbeat", {
			method: "POST",
			headers: {
				"content-type": "application/json",
				authorization: `Bearer ${workerJwt}`,
			},
			body: JSON.stringify(body),
		}),
	);
}

describe("WorkersHeartbeat reachable_addr backfill", () => {
	test("a beat carrying reachable_addr persists the LIVE DNSName to the row", async () => {
		const resp = await heartbeat({
			reachableAddr: "coord-host.tailXXXXXX.ts.net",
		});
		expect(resp.status).toBe(200);
		const row = await db
			.selectFrom("workers")
			.select(["reachable_addr"])
			.where("fp", "=", workerFp)
			.executeTakeFirstOrThrow();
		expect(row.reachable_addr).toBe("coord-host.tailXXXXXX.ts.net");
	});

	test("an empty reachable_addr keeps the prior value (tailscale unreachable this beat)", async () => {
		await heartbeat({ reachableAddr: "coord-host.tailXXXXXX.ts.net" });
		const resp = await heartbeat({ reachableAddr: "" });
		expect(resp.status).toBe(200);
		const row = await db
			.selectFrom("workers")
			.select(["reachable_addr"])
			.where("fp", "=", workerFp)
			.executeTakeFirstOrThrow();
		expect(row.reachable_addr).toBe("coord-host.tailXXXXXX.ts.net");
	});

	test("a beat with NO reachable_addr field also keeps the prior value", async () => {
		await heartbeat({ reachableAddr: "coord-host.tailXXXXXX.ts.net" });
		const resp = await heartbeat({ gitSha: "deadbeef" });
		expect(resp.status).toBe(200);
		const row = await db
			.selectFrom("workers")
			.select(["reachable_addr"])
			.where("fp", "=", workerFp)
			.executeTakeFirstOrThrow();
		expect(row.reachable_addr).toBe("coord-host.tailXXXXXX.ts.net");
	});

	test("a CHANGED reachable_addr republishes worker presence (SPA sees the new address)", async () => {
		// Seed a known prior value, then flip it and assert presenceBus fires a
		// 'registered' delta so the SPA store updates without a manual refresh.
		await heartbeat({ reachableAddr: "host.tailXXXXXX.ts.net" });
		const seen: string[] = [];
		const unsub = presenceBus.subscribe((msg: { kind: string }) => {
			seen.push(msg.kind);
		});
		try {
			await heartbeat({ reachableAddr: "coord-host.tailXXXXXX.ts.net" });
		} finally {
			unsub();
		}
		expect(seen).toContain("registered");
	});
});

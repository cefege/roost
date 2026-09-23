/**
 * Pins additive 0033 pairing migration against the shipped 0032 table shape.
 * Legacy pending rows fail closed while terminal rows and provenance survive.
 * Table and index root pages prove no replacement rebuild occurred.
 */
import { expect, test } from "bun:test";
import type { Database } from "bun:sqlite";
import {
  apply0033PairVerificationMigration,
  open0032PairRequestDatabase,
  open0033PairRequestDatabase,
  seedLegacyPairRequest,
} from "./pair-verification-migration-fixture.ts";

const PENDING_REQUEST = {
  id: "legacy-pending-request",
  ephemeralId: "legacy-pending-correlation",
  publicKey: new Uint8Array(32).fill(0xa5),
  label: "Legacy Chromium on macOS",
  status: "pending" as const,
  createdAtMs: 1_700_000_000_000,
  decidedAtMs: null,
  provenance: {
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_4) Chrome/124.0.0.0",
    clientBrowser: "Chrome",
    clientOs: "macOS",
    clientDeviceType: "desktop",
    sourceIp: "203.0.113.47",
    countryCode: "DE",
    region: "Berlin",
    city: "Berlin",
    edgeIdentityProvider: "cloudflare-access",
    edgeIdentity: "browser@example.test",
    edgeIdentityVerified: 1,
  },
  expiresAtMs: 1_700_000_600_000,
};

const APPROVED_REQUEST = {
  id: "legacy-approved-request",
  ephemeralId: "legacy-approved-correlation",
  publicKey: new Uint8Array(32).fill(0x5a),
  label: "Legacy Firefox on Linux",
  status: "approved" as const,
  createdAtMs: 1_699_999_000_000,
  decidedAtMs: 1_699_999_060_000,
  provenance: {
    userAgent: "Mozilla/5.0 (X11; Linux x86_64; rv:124.0) Firefox/124.0",
    clientBrowser: "Firefox",
    clientOs: "Linux",
    clientDeviceType: "desktop",
    sourceIp: "198.51.100.19",
    countryCode: "US",
    region: "Oregon",
    city: "Portland",
    edgeIdentityProvider: "tailscale",
    edgeIdentity: "node-legacy",
    edgeIdentityVerified: 1,
  },
  expiresAtMs: 1_699_999_600_000,
};

type MigratedPairRequest = {
  id: string;
  ephemeral_id: string;
  public_key_hex: string;
  label: string;
  status: string;
  created_at_ms: number;
  decided_at_ms: number | null;
  ceremony_version: number;
  requester_token_hash: string;
  verification_code_hash: string | null;
  verification_attempts: number;
  approved_by_fp: string | null;
  approved_account_id: string | null;
  user_agent: string | null;
  client_browser: string | null;
  client_os: string | null;
  client_device_type: string | null;
  source_ip: string | null;
  country_code: string | null;
  region: string | null;
  city: string | null;
  edge_identity_provider: string | null;
  edge_identity: string | null;
  edge_identity_verified: number;
  expires_at_ms: number;
};

function pairRequestColumns(sqlite: Database): string[] {
  const statement = sqlite.prepare("PRAGMA table_info(pair_requests)");
  try {
    return (statement.all() as Array<{ name: string }>).map(({ name }) => name);
  } finally {
    statement.finalize();
  }
}

function schemaRootPage(sqlite: Database, type: "index" | "table", name: string): number {
  const statement = sqlite.prepare(`
    SELECT rootpage FROM sqlite_master WHERE type = ? AND name = ?
  `);
  try {
    const row = statement.get(type, name) as { rootpage: number } | null;
    if (row === null) throw new Error(`missing ${type}: ${name}`);
    return row.rootpage;
  } finally {
    statement.finalize();
  }
}

function readMigratedPairRequest(sqlite: Database, id: string): MigratedPairRequest {
  const statement = sqlite.prepare(`
    SELECT
      id,
      ephemeral_id,
      hex(public_key) AS public_key_hex,
      label,
      status,
      created_at_ms,
      decided_at_ms,
      ceremony_version,
      requester_token_hash,
      verification_code_hash,
      verification_attempts,
      approved_by_fp,
      approved_account_id,
      user_agent,
      client_browser,
      client_os,
      client_device_type,
      source_ip,
      country_code,
      region,
      city,
      edge_identity_provider,
      edge_identity,
      edge_identity_verified,
      expires_at_ms
    FROM pair_requests
    WHERE id = ?
  `);
  try {
    const row = statement.get(id) as MigratedPairRequest | null;
    if (row === null) throw new Error(`missing migrated pair request: ${id}`);
    return row;
  } finally {
    statement.finalize();
  }
}

function statusExpiryIndexColumns(sqlite: Database): Array<{ seqno: number; name: string }> {
  const statement = sqlite.prepare("PRAGMA index_info(idx_pair_requests_status_expires)");
  try {
    return (statement.all() as Array<{ seqno: number; name: string }>)
      .map(({ seqno, name }) => ({ seqno, name }));
  } finally {
    statement.finalize();
  }
}

function indexedPairRequestIds(
  sqlite: Database,
  status: string,
  expiresAtMs: number,
): string[] {
  const statement = sqlite.prepare(`
    SELECT ephemeral_id
    FROM pair_requests INDEXED BY idx_pair_requests_status_expires
    WHERE status = ? AND expires_at_ms = ?
    ORDER BY ephemeral_id
  `);
  try {
    return (statement.all(status, expiresAtMs) as Array<{ ephemeral_id: string }>)
      .map(({ ephemeral_id }) => ephemeral_id);
  } finally {
    statement.finalize();
  }
}

test("0033 adds verifier fields in place while expiring legacy pending pair requests", async () => {
  const sqlite = await open0032PairRequestDatabase();
  try {
    expect(sqlite.query(
      "SELECT name FROM _migrations ORDER BY name DESC LIMIT 1",
    ).get()).toEqual({ name: "0032_workers_mecatl_runtime" });
    expect(pairRequestColumns(sqlite)).toEqual([
      "id",
      "ephemeral_id",
      "public_key",
      "label",
      "status",
      "created_at_ms",
      "decided_at_ms",
      "user_agent",
      "client_browser",
      "client_os",
      "client_device_type",
      "source_ip",
      "country_code",
      "region",
      "city",
      "edge_identity_provider",
      "edge_identity",
      "edge_identity_verified",
      "expires_at_ms",
    ]);
    seedLegacyPairRequest(sqlite, PENDING_REQUEST);
    seedLegacyPairRequest(sqlite, APPROVED_REQUEST);

    expect(sqlite.query(`
      SELECT status, expires_at_ms
      FROM pair_requests
      WHERE id = ?
    `).get(PENDING_REQUEST.id)).toEqual({
      status: "pending",
      expires_at_ms: PENDING_REQUEST.expiresAtMs,
    });
    expect(statusExpiryIndexColumns(sqlite)).toEqual([
      { seqno: 0, name: "status" },
      { seqno: 1, name: "expires_at_ms" },
    ]);
    expect(indexedPairRequestIds(
      sqlite,
      "pending",
      PENDING_REQUEST.expiresAtMs,
    )).toEqual([PENDING_REQUEST.ephemeralId]);
    const tableRootPageBefore = schemaRootPage(sqlite, "table", "pair_requests");
    const statusExpiryIndexRootPageBefore = schemaRootPage(
      sqlite,
      "index",
      "idx_pair_requests_status_expires",
    );

    await apply0033PairVerificationMigration(sqlite);

    const pendingRequest = readMigratedPairRequest(sqlite, PENDING_REQUEST.id);
    const { decided_at_ms: pendingDecisionAtMs, ...pendingFields } = pendingRequest;
    expect(pendingDecisionAtMs).not.toBeNull();
    expect(pendingFields).toEqual({
      id: PENDING_REQUEST.id,
      ephemeral_id: PENDING_REQUEST.ephemeralId,
      public_key_hex: "A5".repeat(32),
      label: PENDING_REQUEST.label,
      status: "expired",
      created_at_ms: PENDING_REQUEST.createdAtMs,
      ceremony_version: 0,
      requester_token_hash: "",
      verification_code_hash: null,
      verification_attempts: 0,
      approved_by_fp: null,
      approved_account_id: null,
      user_agent: PENDING_REQUEST.provenance.userAgent,
      client_browser: PENDING_REQUEST.provenance.clientBrowser,
      client_os: PENDING_REQUEST.provenance.clientOs,
      client_device_type: PENDING_REQUEST.provenance.clientDeviceType,
      source_ip: PENDING_REQUEST.provenance.sourceIp,
      country_code: PENDING_REQUEST.provenance.countryCode,
      region: PENDING_REQUEST.provenance.region,
      city: PENDING_REQUEST.provenance.city,
      edge_identity_provider: PENDING_REQUEST.provenance.edgeIdentityProvider,
      edge_identity: PENDING_REQUEST.provenance.edgeIdentity,
      edge_identity_verified: PENDING_REQUEST.provenance.edgeIdentityVerified,
      expires_at_ms: PENDING_REQUEST.expiresAtMs,
    });
    expect(readMigratedPairRequest(sqlite, APPROVED_REQUEST.id)).toEqual({
      id: APPROVED_REQUEST.id,
      ephemeral_id: APPROVED_REQUEST.ephemeralId,
      public_key_hex: "5A".repeat(32),
      label: APPROVED_REQUEST.label,
      status: "approved",
      created_at_ms: APPROVED_REQUEST.createdAtMs,
      decided_at_ms: APPROVED_REQUEST.decidedAtMs,
      ceremony_version: 0,
      requester_token_hash: "",
      verification_code_hash: null,
      verification_attempts: 0,
      approved_by_fp: null,
      approved_account_id: null,
      user_agent: APPROVED_REQUEST.provenance.userAgent,
      client_browser: APPROVED_REQUEST.provenance.clientBrowser,
      client_os: APPROVED_REQUEST.provenance.clientOs,
      client_device_type: APPROVED_REQUEST.provenance.clientDeviceType,
      source_ip: APPROVED_REQUEST.provenance.sourceIp,
      country_code: APPROVED_REQUEST.provenance.countryCode,
      region: APPROVED_REQUEST.provenance.region,
      city: APPROVED_REQUEST.provenance.city,
      edge_identity_provider: APPROVED_REQUEST.provenance.edgeIdentityProvider,
      edge_identity: APPROVED_REQUEST.provenance.edgeIdentity,
      edge_identity_verified: APPROVED_REQUEST.provenance.edgeIdentityVerified,
      expires_at_ms: APPROVED_REQUEST.expiresAtMs,
    });
    expect(schemaRootPage(sqlite, "table", "pair_requests")).toBe(tableRootPageBefore);
    expect(schemaRootPage(
      sqlite,
      "index",
      "idx_pair_requests_status_expires",
    )).toBe(statusExpiryIndexRootPageBefore);

    expect(sqlite.query(`
      SELECT name FROM sqlite_master
      WHERE type = 'index' AND name = 'idx_pair_requests_status_expires'
    `).get()).toEqual({ name: "idx_pair_requests_status_expires" });
    expect(statusExpiryIndexColumns(sqlite)).toEqual([
      { seqno: 0, name: "status" },
      { seqno: 1, name: "expires_at_ms" },
    ]);
    expect(indexedPairRequestIds(
      sqlite,
      "expired",
      PENDING_REQUEST.expiresAtMs,
    )).toEqual([PENDING_REQUEST.ephemeralId]);
  } finally {
    sqlite.close(true);
  }
});

test("0033 completes the unmodified empty migration history", async () => {
  const sqlite = await open0033PairRequestDatabase();
  try {
    expect(sqlite.query(
      "SELECT name FROM _migrations ORDER BY name DESC LIMIT 1",
    ).get()).toEqual({ name: "0033_pair_verification_code" });
    expect(pairRequestColumns(sqlite)).toEqual([
      "id",
      "ephemeral_id",
      "public_key",
      "label",
      "status",
      "created_at_ms",
      "decided_at_ms",
      "user_agent",
      "client_browser",
      "client_os",
      "client_device_type",
      "source_ip",
      "country_code",
      "region",
      "city",
      "edge_identity_provider",
      "edge_identity",
      "edge_identity_verified",
      "expires_at_ms",
      "ceremony_version",
      "requester_token_hash",
      "verification_code_hash",
      "verification_attempts",
      "approved_by_fp",
      "approved_account_id",
    ]);
    expect(sqlite.query("SELECT count(*) AS count FROM pair_requests").get()).toEqual({
      count: 0,
    });
    expect(schemaRootPage(sqlite, "table", "pair_requests")).toBeGreaterThan(0);
    expect(schemaRootPage(
      sqlite,
      "index",
      "idx_pair_requests_status_expires",
    )).toBeGreaterThan(0);
  } finally {
    sqlite.close(true);
  }
});

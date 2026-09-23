/**
 * Replays the shipped migration chain through 0032 before applying 0033.
 * It seeds only rows an existing coordinator could have persisted.
 * Every SQL body comes from the migration files under test.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { runMigrations } from "../src/db/migrate.ts";

const migrationsDir = join(import.meta.dir, "../migrations");
const lastLegacyMigrationFile = "0032_workers_mecatl_runtime.sql";
const pairVerificationMigrationFile = "0033_pair_verification_code.sql";

export interface LegacyPairRequestProvenance {
  userAgent: string | null;
  clientBrowser: string | null;
  clientOs: string | null;
  clientDeviceType: string | null;
  sourceIp: string | null;
  countryCode: string | null;
  region: string | null;
  city: string | null;
  edgeIdentityProvider: string | null;
  edgeIdentity: string | null;
  edgeIdentityVerified: number;
}

export interface LegacyPairRequest {
  id: string;
  ephemeralId: string;
  publicKey: Uint8Array;
  label: string;
  status: "pending" | "approved";
  createdAtMs: number;
  decidedAtMs: number | null;
  provenance: LegacyPairRequestProvenance;
  expiresAtMs: number;
}

const migrationFiles = readdirSync(migrationsDir)
  .filter((file) => file.endsWith(".sql"))
  .sort();

function migrationsThrough(lastMigrationFile: string): Array<{ name: string; sql: string }> {
  if (!migrationFiles.includes(lastMigrationFile)) {
    throw new Error(`${lastMigrationFile} is missing`);
  }
  return migrationFiles
    .filter((file) => file <= lastMigrationFile)
    .map((file) => ({
      name: file.slice(0, -4),
      sql: readFileSync(join(migrationsDir, file), "utf8"),
    }));
}

const migrationsThrough0032 = migrationsThrough(lastLegacyMigrationFile);
const migrationsThrough0033 = migrationsThrough(pairVerificationMigrationFile);

export async function open0032PairRequestDatabase(): Promise<Database> {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  await runMigrations(sqlite, migrationsThrough0032);
  return sqlite;
}

export async function open0033PairRequestDatabase(): Promise<Database> {
  const sqlite = new Database(":memory:");
  sqlite.exec("PRAGMA foreign_keys = ON");
  await runMigrations(sqlite, migrationsThrough0033);
  return sqlite;
}

export async function apply0033PairVerificationMigration(sqlite: Database): Promise<void> {
  await runMigrations(sqlite, migrationsThrough0033);
}

export function seedLegacyPairRequest(sqlite: Database, request: LegacyPairRequest): void {
  const statement = sqlite.prepare(`
    INSERT INTO pair_requests (
      id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms,
      user_agent, client_browser, client_os, client_device_type, source_ip,
      country_code, region, city, edge_identity_provider, edge_identity,
      edge_identity_verified, expires_at_ms
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `);
  try {
    statement.run(
      request.id,
      request.ephemeralId,
      request.publicKey,
      request.label,
      request.status,
      request.createdAtMs,
      request.decidedAtMs,
      request.provenance.userAgent,
      request.provenance.clientBrowser,
      request.provenance.clientOs,
      request.provenance.clientDeviceType,
      request.provenance.sourceIp,
      request.provenance.countryCode,
      request.provenance.region,
      request.provenance.city,
      request.provenance.edgeIdentityProvider,
      request.provenance.edgeIdentity,
      request.provenance.edgeIdentityVerified,
      request.expiresAtMs,
    );
  } finally {
    statement.finalize();
  }
}

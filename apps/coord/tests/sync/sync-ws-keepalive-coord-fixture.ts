/**
 * Owns the database and identity fixture shared by Sync WebSocket keepalive suites.
 * Each discovered suite creates and closes its own instance so mutations cannot leak across files.
 * It depends on real coordinator migrations, key generation, JWT signing, and the self-hosted tenant.
 */
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CoordConfig } from "@roost/host/config";
import { fingerprintOf } from "@roost/protocol/fingerprint";
import type { ConnectDeps } from "../../src/rpc/router.ts";
import { CoordinatorWriteGate } from "../../src/coordinator-write-gate.ts";
import { openDb } from "../../src/db/connection.ts";
import { runMigrations } from "../../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../../src/auth/self-hosted-tenant.ts";
import { newJwtCache, signJwt } from "../../src/auth/jwt.ts";
import { UiLayoutApplyOwner } from "../../src/ui-state/ui-layout-apply-owner.ts";
import { UiStateOwner } from "../../src/ui-state/ui-state-owner.ts";
import { TerminalGrantOwner } from "../../src/terminal/direct/terminal-grant-owner.ts";
import { AttachmentGrantOwner } from "../../src/attachments/attachment-grant-owner.ts";
import { TerminalPeerNegotiations } from "../../src/terminal/direct/terminal-peer-negotiations.ts";
import { AttachmentPeerNegotiations } from "../../src/attachments/attachment-peer-negotiations.ts";
import { AttachmentDirectStatusResults } from "../../src/attachments/attachment-direct-status-results.ts";

export interface SyncWsKeepaliveCoordFixture {
  deps: ConnectDeps;
  fingerprint: string;
  jwt: string;
  close(): Promise<void>;
}

export async function createSyncWsKeepaliveCoordFixture(): Promise<SyncWsKeepaliveCoordFixture> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-sync-keepalive-"));
  const dbPath = join(workdir, "test.db");
  const authorizedKeysPath = join(workdir, "authorized_keys");
  writeFileSync(authorizedKeysPath, "");

  const opened = openDb(dbPath);
  const { db, sqlite } = opened;
  await runMigrations(sqlite);
  const selfHostedTenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    pushAllowedOrigins: [],
    bind: "127.0.0.1:0",
    dbPath,
    authorizedKeysPath,
    webDistPath: "",
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    trustProxy: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    webPublicUrl: "https://public.example",
    publicUrl: undefined,
    terminalPeerEnabled: false,
    terminalPeerStunUrls: [],
  };
  const terminalGrants = new TerminalGrantOwner();
  const attachmentGrants = new AttachmentGrantOwner();
  const terminalPeerNegotiations = new TerminalPeerNegotiations({
    db,
    cfg,
    terminalGrants,
  });
  const attachmentPeerNegotiations = new AttachmentPeerNegotiations({
    cfg,
    attachmentGrants,
  });
  const attachmentDirectStatusResults = new AttachmentDirectStatusResults();
  const deps: ConnectDeps = {
    db,
    sqlite,
    writeGate: new CoordinatorWriteGate(),
    jwtCache,
    cfg,
    uiLayoutApplies: new UiLayoutApplyOwner(),
    uiStates: new UiStateOwner(),
    selfHostedTenant,
    cfAccess: null,
    terminalGrants,
    terminalPeerNegotiations,
    attachmentGrants,
    attachmentPeerNegotiations,
    attachmentDirectStatusResults,
  };

  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPublicKey = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  const fingerprint = await fingerprintOf(rawPublicKey);
  await db.insertInto("authorized_keys").values({
    fingerprint,
    public_key: rawPublicKey,
    label: "test-web",
    added_at: Date.now(),
  }).execute();
  const membershipNow = Date.now();
  await db.insertInto("account_devices").values({
    fingerprint,
    account_id: selfHostedTenant.accountId,
    added_at_ms: membershipNow,
    last_seen_at_ms: membershipNow,
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  const jwt = await signJwt(
    { aud: "roost-coordinator", sub: fingerprint, iat: now, exp: now + 60 },
    keys.privateKey,
    fingerprint,
  );

  return {
    deps,
    fingerprint,
    jwt,
    async close() {
      deps.uiLayoutApplies.dispose();
      deps.uiStates.dispose();
      terminalPeerNegotiations.dispose();
      attachmentDirectStatusResults.dispose();
      attachmentPeerNegotiations.dispose();
      attachmentGrants.dispose();
      terminalGrants.dispose();
      try {
        await opened.close();
      } finally {
        if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
      }
    },
  };
}

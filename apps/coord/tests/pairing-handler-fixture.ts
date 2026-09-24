// Real pairing-handler tests share one isolated account-device coordinator
// fixture. It owns generated-method typing, caller contexts, pairBus capture,
// requester ceremony values, migrations, and deterministic teardown.

import { create } from "@bufbuild/protobuf";
import { createContextValues } from "@connectrpc/connect";
import type { HandlerContext, ServiceImpl } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/protocol/fingerprint";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairRequestId,
  generatePairRequesterToken,
} from "@roost/protocol/pairing";
import {
  CoordinatorService,
  PairCreateRequestSchema,
  type PairApprovalStatusResponse,
  type PairApproveResponse,
  type PairConfirmResponse,
  type PairCreateResponse,
  type PairDenyResponse,
  type PairListResponse,
  type PairPollResponse,
} from "@roost/protocol/proto/coordinator_pb";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  onHostKey,
  remoteAddressKey,
} from "../src/connect/auth-interceptor.ts";
import { makeAuthHandlers } from "../src/connect/handlers-auth.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { pairBus, type PairRequestDelta } from "../src/buses.ts";
import { openDb, type KyselyDB } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { newJwtCache } from "../src/jwt.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";

type PairMethod =
  | "pairCreate"
  | "pairPoll"
  | "pairApprove"
  | "pairConfirm"
  | "pairDeny"
  | "pairList"
  | "pairApprovalStatus";
type PairResponse = {
  pairCreate: PairCreateResponse;
  pairPoll: PairPollResponse;
  pairApprove: PairApproveResponse;
  pairConfirm: PairConfirmResponse;
  pairDeny: PairDenyResponse;
  pairList: PairListResponse;
  pairApprovalStatus: PairApprovalStatusResponse;
};
export type PairHandlers = {
  [Method in PairMethod]: (
    ...args: Parameters<ServiceImpl<typeof CoordinatorService>[Method]>
  ) => Promise<PairResponse[Method]>;
};

export interface CreatedPairRequest {
  ephemeralId: string;
  requesterToken: string;
  publicKey: Uint8Array;
  fingerprint: string;
}

export interface CreatePairRequestOptions {
  ephemeralId?: string;
  requesterToken?: string;
  publicKey?: Uint8Array;
}

export interface PairingHandlerHarness {
  accountId: string;
  dashboardId: string;
  authContext: HandlerContext;
  db: KyselyDB;
  handlers: PairHandlers;
  capture(): { messages: PairRequestDelta[]; stop: () => void };
  close(): Promise<void>;
  createRequest(
    label: string,
    options?: CreatePairRequestOptions,
  ): Promise<CreatedPairRequest>;
  remoteContext(address: string | undefined, onHost?: boolean): HandlerContext;
}

export async function openPairingHandlerHarness(): Promise<PairingHandlerHarness> {
  const workdir = mkdtempSync(join(tmpdir(), "roost-pairbus-"));
  const opened = openDb(join(workdir, "test.db"));
  const { db } = opened;
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  const now = Date.now();
  await db.insertInto("authorized_keys").values({
    fingerprint: "fp-test",
    public_key: new Uint8Array(32),
    label: "approver",
    added_at: now,
  }).execute();
  await db.insertInto("account_devices").values({
    fingerprint: "fp-test",
    account_id: tenant.accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).execute();
  const authContext = {
    values: {
      get: () => ({
        kind: "account-device",
        fingerprint: "fp-test",
        label: "test",
        accountId: tenant.accountId,
      }),
    },
  } as unknown as HandlerContext;
  const handlers = makeAuthHandlers({
    db,
    jwtCache: newJwtCache(),
    cfg: {},
    selfHostedTenant: tenant,
  } as unknown as ConnectDeps) as unknown as PairHandlers;

  return {
    accountId: tenant.accountId,
    dashboardId: tenant.dashboardId,
    authContext,
    db,
    handlers,
    capture() {
      const messages: PairRequestDelta[] = [];
      const stop = pairBus.subscribe((message) => messages.push(message));
      return { messages, stop };
    },
    async close() {
      try {
        await opened.close();
      } finally {
        rmSync(workdir, { recursive: true, force: true });
      }
    },
    async createRequest(label, options = {}) {
      const publicKey = options.publicKey ?? await generateRequesterPublicKey();
      const ephemeralId = options.ephemeralId ?? generatePairRequestId();
      const requesterToken = options.requesterToken ?? generatePairRequesterToken();
      const response = await handlers.pairCreate(create(PairCreateRequestSchema, {
        sshPubkeyB64: Buffer.from(publicKey).toString("base64"),
        label,
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId,
        requesterToken,
      }), authContext);
      if (response.ephemeralId !== ephemeralId) {
        throw new Error("pair creation returned a different request id");
      }
      return {
        ephemeralId,
        requesterToken,
        publicKey,
        fingerprint: await fingerprintOf(publicKey),
      };
    },
    remoteContext(address, onHost = false) {
      const values = createContextValues();
      if (address !== undefined) values.set(remoteAddressKey, address);
      values.set(onHostKey, onHost);
      return { values } as unknown as HandlerContext;
    },
  };
}

async function generateRequesterPublicKey(): Promise<Uint8Array> {
  const keys = await crypto.subtle.generateKey(
    { name: "Ed25519" },
    true,
    ["sign", "verify"],
  );
  return new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
}

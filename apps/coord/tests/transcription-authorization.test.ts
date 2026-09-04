import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, type HandlerContext } from "@connectrpc/connect";
import { TranscriptionGrantTokenRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  callerKey,
  dashboardActorKey,
  type DashboardRole,
} from "../src/connect/auth-interceptor.ts";
import { makeTranscriptionHandlers } from "../src/connect/handlers-transcription.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const DEEPGRAM_KEY = "configured-owner-deepgram-key";
const DEVICE_FP = "owner-device";
const DASHBOARD_ID = "owner-dashboard";

function browserContext(dashboardRole: DashboardRole): HandlerContext {
  const caller = {
    kind: "account-device" as const,
    fingerprint: DEVICE_FP,
    label: "Owner browser",
    accountId: "owner-account",
  };
  const actor = {
    accountId: caller.accountId,
    organizationId: "owner-organization",
    dashboardId: DASHBOARD_ID,
    organizationRole: "owner" as const,
    dashboardRole,
    deviceFingerprint: DEVICE_FP,
  };
  return {
    values: {
      get: (key: unknown) => {
        if (key === callerKey) return caller;
        if (key === dashboardActorKey) return actor;
        return undefined;
      },
    },
  } as unknown as HandlerContext;
}

function transcriptionDb(): { db: ConnectDeps["db"]; reads: () => number } {
  let readCount = 0;
  const query = {
    select: () => query,
    where: () => query,
    execute: async () => {
      readCount++;
      return [{ key: "transcription.deepgram_key", value: DEEPGRAM_KEY }];
    },
  };
  return {
    db: {
      selectFrom: () => query,
    } as unknown as ConnectDeps["db"],
    reads: () => readCount,
  };
}

test("TranscriptionGrantToken returns the stored key only to a dashboard admin", async () => {
  const database = transcriptionDb();
  const handlers = makeTranscriptionHandlers({ db: database.db } as ConnectDeps);
  const request = create(TranscriptionGrantTokenRequestSchema);

  await expect(handlers.transcriptionGrantToken(
    request,
    browserContext("member"),
  )).rejects.toMatchObject({ code: Code.PermissionDenied });
  expect(database.reads()).toBe(0);

  await expect(handlers.transcriptionGrantToken(
    request,
    browserContext("admin"),
  )).resolves.toMatchObject({
    accessToken: DEEPGRAM_KEY,
    expiresIn: 0,
  });
  expect(database.reads()).toBe(1);
});

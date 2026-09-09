// The Deepgram key handoff hands a stored secret to the caller, so the
// authority check must run before the settings read. A worker JWT and an
// anonymous request must both be refused without the key ever being loaded.

import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { Code, type HandlerContext } from "@connectrpc/connect";
import { TranscriptionGrantTokenRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { callerKey, type Caller } from "../src/connect/auth-interceptor.ts";
import { makeTranscriptionHandlers } from "../src/connect/handlers-transcription.ts";
import type { ConnectDeps } from "../src/connect/router.ts";

const DEEPGRAM_KEY = "configured-owner-deepgram-key";
const DEVICE_FP = "owner-device";
const WORKER_FP = "worker-fingerprint";

function contextFor(caller: Caller | null): HandlerContext {
  return {
    values: {
      get: (key: unknown) => (key === callerKey ? caller : undefined),
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

test("TranscriptionGrantToken returns the stored key only to a browser device", async () => {
  const database = transcriptionDb();
  const handlers = makeTranscriptionHandlers({ db: database.db } as ConnectDeps);
  const request = create(TranscriptionGrantTokenRequestSchema);

  for (const refused of [
    null,
    { kind: "worker" as const, fingerprint: WORKER_FP, label: "worker" },
  ]) {
    await expect(handlers.transcriptionGrantToken(
      request,
      contextFor(refused),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
  }
  expect(database.reads()).toBe(0);

  await expect(handlers.transcriptionGrantToken(
    request,
    contextFor({
      kind: "account-device",
      fingerprint: DEVICE_FP,
      label: "Owner browser",
      accountId: "owner-account",
    }),
  )).resolves.toMatchObject({
    accessToken: DEEPGRAM_KEY,
    expiresIn: 0,
  });
  expect(database.reads()).toBe(1);
});

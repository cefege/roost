// Transcription (voice-dictation) RPC handlers — Deepgram config + token grant.
// Registered into the CoordinatorService via its own router.service() call in
// router.ts::buildConnectRouter. Closes over ConnectDeps only (deps.db); no
// shared router-local state. Split out of router.ts (400-line cap; the
// "all handlers in one router.ts" rule was rescinded 2026-06-23).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  TranscriptionConfigSchema,
  TranscriptionGrantTokenResponseSchema,
  TranscriptionTestResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { requireAuth } from "./auth-interceptor.ts";
import {
  getTranscriptionConfig, setTranscriptionConfig, grantDeepgramToken, testDeepgram,
} from "../transcription.ts";
import type { ConnectDeps } from "./router.ts";

type TranscriptionMethods =
  | "transcriptionGetConfig" | "transcriptionSetConfig"
  | "transcriptionGrantToken" | "transcriptionTest";

export function makeTranscriptionHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, TranscriptionMethods> {
  return {
    async transcriptionGetConfig(_req, ctx) {
      requireAuth(ctx.values);
      const c = await getTranscriptionConfig(deps.db);
      return create(TranscriptionConfigSchema, c);
    },

    async transcriptionSetConfig(req, ctx) {
      requireAuth(ctx.values);
      const c = await setTranscriptionConfig(deps.db, {
        deepgramKey: req.deepgramKey,
        deepgramLanguage: req.deepgramLanguage,
      });
      return create(TranscriptionConfigSchema, c);
    },

    async transcriptionGrantToken(_req, ctx) {
      requireAuth(ctx.values);
      try {
        const { accessToken, expiresIn } = await grantDeepgramToken(deps.db);
        return create(TranscriptionGrantTokenResponseSchema, { accessToken, expiresIn });
      } catch (err) {
        const msg = String(err instanceof Error ? err.message : err);
        if (msg === "deepgram_not_configured") {
          throw new ConnectError("Deepgram not configured", Code.FailedPrecondition);
        }
        throw new ConnectError(`Deepgram token grant failed (${msg})`, Code.Unavailable);
      }
    },

    async transcriptionTest(_req, ctx) {
      requireAuth(ctx.values);
      const { ok, error } = await testDeepgram(deps.db);
      return create(TranscriptionTestResponseSchema, { ok, error });
    },
  };
}

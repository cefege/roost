// Transcription RPC handlers — Deepgram config plus the explicit stored-key handoff.
// Spread into router.ts's SINGLE router.service() literal — never registered
// with a router.service() call of its own, which would shadow every other
// domain with unimplemented-throws. Closes over ConnectDeps only (deps.db); no
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
import { requireDashboardActor, requireDashboardAdmin } from "./auth-interceptor.ts";
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
      const actor = requireDashboardActor(ctx.values);
      const c = await getTranscriptionConfig(deps.db, actor.dashboardId);
      return create(TranscriptionConfigSchema, c);
    },

    async transcriptionSetConfig(req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      const c = await setTranscriptionConfig(deps.db, actor.dashboardId, {
        deepgramKey: req.deepgramKey,
        deepgramLanguage: req.deepgramLanguage,
      });
      return create(TranscriptionConfigSchema, c);
    },

    // Direct Deepgram mode cannot mint a restricted temporary grant. Return the
    // configured key only to a dashboard admin (the sole owner in managed mode),
    // whose browser connects to Deepgram directly.
    async transcriptionGrantToken(_req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      try {
        const { accessToken, expiresIn } = await grantDeepgramToken(deps.db, actor.dashboardId);
        return create(TranscriptionGrantTokenResponseSchema, { accessToken, expiresIn });
      } catch (err) {
        const msg = String(err instanceof Error ? err.message : err);
        if (msg === "deepgram_not_configured") {
          throw new ConnectError("Deepgram not configured", Code.FailedPrecondition);
        }
        throw new ConnectError(`Deepgram key handoff failed (${msg})`, Code.Unavailable);
      }
    },

    async transcriptionTest(_req, ctx) {
      const actor = requireDashboardAdmin(ctx.values);
      const { ok, error } = await testDeepgram(deps.db, actor.dashboardId);
      return create(TranscriptionTestResponseSchema, { ok, error });
    },
  };
}

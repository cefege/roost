// Sync firehose — one server-streaming RPC for in-memory state buses,
// including cell frames, compact terminal-link metadata, and browser UI coordination.
// Only the retired RPC stub lives here now; the feed itself is sync-feed.ts.

import type { ServiceImpl } from "@connectrpc/connect";
import { ConnectError, Code } from "@connectrpc/connect";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { requireAuth } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type StreamingMethods = "sync";

export function makeStreamingHandlers(
  _deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, StreamingMethods> {
  return {
    // The Sync firehose moved to a raw Bun WebSocket at /ws/coord-sync
    // (sync-ws-handler.ts) to dodge the Bun 1.3.14 use-after-free in
    // RequestContext.onAbort that crashed coord whenever a browser aborted
    // this long-lived streaming response. The feed lives in startSyncFeed
    // (sync-feed.ts), now consumed ONLY by the WS handler; this Connect method stays
    // declared for ServiceImpl completeness but is unimplemented so the
    // crashing abort-listener path is GONE, not merely unused.
    async *sync(_req, ctx): AsyncGenerator<FirehoseFrame> {
      requireAuth(ctx.values);
      throw new ConnectError("sync moved to /ws/coord-sync", Code.Unimplemented);
    },
  };
}

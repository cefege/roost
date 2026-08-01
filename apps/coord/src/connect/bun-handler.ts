// Bun.serve ↔ Connect adapter. Converts Fetch Request → UniversalServerRequest,
// dispatches to a ConnectRouter's matched handler, returns Fetch Response.
//
// Connect's `connectNodeAdapter` expects Node http.Server; we use Bun.serve
// fetch handlers. UniversalServerRequest/Response are protocol-neutral, so
// this adapter is ~60 lines.
//
// Path matching: each handler exposes `requestPath` (e.g.
// "/roost.v1.CoordinatorService/WorkersList"). We dispatch on
// `url.pathname` exact match. Bun.serve guarantees one fetch call per request.

import type { ConnectRouter } from "@connectrpc/connect";
import type { UniversalHandler } from "@connectrpc/connect/protocol";
import type { UniversalServerRequest, UniversalServerResponse } from "@connectrpc/connect/protocol";

export interface ConnectBunHandler {
  /** True if the path matches a registered Connect RPC. */
  matches(pathname: string): boolean;
  /** Dispatch a Fetch Request → Fetch Response. Caller must already have matched. */
  fetch(req: Request, clientAddress?: string): Promise<Response>;
}

export function makeConnectBunHandler(router: ConnectRouter): ConnectBunHandler {
  const byPath = new Map<string, UniversalHandler>();
  for (const h of router.handlers) byPath.set(h.requestPath, h);

  function matches(pathname: string): boolean {
    return byPath.has(pathname);
  }

  async function fetchHandler(req: Request, clientAddress?: string): Promise<Response> {
    const url = new URL(req.url);
    const handler = byPath.get(url.pathname);
    if (!handler) return new Response("not found", { status: 404 });

    const allowedMethods = handler.allowedMethods ?? ["POST"];
    if (!allowedMethods.includes(req.method)) {
      return new Response("method not allowed", { status: 405 });
    }

    // Copy headers into a fresh Headers instance. Per WHATWG Fetch,
    // server-side fetched Requests can carry an immutable Headers
    // guard (Node http via @connectrpc/connect-node adapter is the
    // documented portability surface for createCoord). Mutating
    // req.headers in place works in Bun but throws "Headers is
    // immutable" elsewhere. The strip-then-set on x-roost-remote-addr
    // below is also a correctness guard: a browser-supplied value
    // must never reach assertLoopback if requestIP() returned null.
    const header = new Headers(req.headers);
    header.delete("x-roost-remote-addr");
    if (clientAddress) header.set("x-roost-remote-addr", clientAddress);

    const ureq: UniversalServerRequest = {
      httpVersion: "2.0",
      url: req.url,
      method: req.method,
      header,
      body: req.body ? toAsyncIterable(req.body) : emptyAsyncIterable(),
      // Bun 1.3.14 has a RequestContext.onAbort use-after-free when JavaScript
      // subscribes to a server Request's signal and the client disconnects.
      // Connect subscribes here, so give each bounded unary RPC a fresh,
      // collectible signal detached from Bun's request lifetime.
      signal: new AbortController().signal,
    };

    const ures: UniversalServerResponse = await handler(ureq);

    const respHeaders = new Headers(ures.header ?? undefined);
    if (ures.trailer) {
      // For unary Connect, trailers are folded into response headers as
      // `trailer-` prefixed entries (Connect protocol spec).
      ures.trailer.forEach((v, k) => respHeaders.append(`trailer-${k}`, v));
    }

    return new Response(
      ures.body ? asyncIterableToReadableStream(ures.body) : null,
      { status: ures.status, headers: respHeaders },
    );
  }

  return { matches, fetch: fetchHandler };
}

function emptyAsyncIterable(): AsyncIterable<Uint8Array> {
  return (async function* () {
    // empty
  })();
}

function toAsyncIterable(stream: ReadableStream<Uint8Array>): AsyncIterable<Uint8Array> {
  return (async function* () {
    const reader = stream.getReader();
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) return;
        if (value) yield value;
      }
    } finally {
      // 2026-06-22: Bun 1.3.14 ReadableStreamDefaultReader has a
      // releaseLock PROPERTY (not method — value is undefined) so ?.()
      // still throws "undefined is not a function". Connect-RPC's
      // transformCatchFinally catches this internally and may corrupt
      // state → Bun segfault at 0x40. typeof check prevents the call
      // entirely when the property is not a callable function.
      if (typeof reader.releaseLock === "function") {
        try { reader.releaseLock(); } catch { /* Bun edge case */ }
      }
    }
  })();
}

function asyncIterableToReadableStream(iter: AsyncIterable<Uint8Array>): ReadableStream<Uint8Array> {
  // Iterator created ONCE; pull() advances it. Earlier version created a
  // fresh iterator per pull which broke streaming after the first chunk.
  let it: AsyncIterator<Uint8Array> | null = null;
  return new ReadableStream<Uint8Array>({
    start() { it = iter[Symbol.asyncIterator](); },
    async pull(controller) {
      if (!it) { controller.close(); return; }
      try {
        const { value, done } = await it.next();
        if (done) {
          // TC39 permits `return X` from an async generator to yield
          // `{value:X, done:true}` as the final tuple. Enqueue the
          // value before closing so a future router stream that uses
          // `return` to ship a terminating frame (trailers, summary)
          // isn't silently dropped. Today's stream handlers only
          // `yield`, so this is latent — fix it before someone adds
          // the first `return`.
          if (value) controller.enqueue(value);
          // Null `it` for symmetry with error and cancel paths below
          // so the exhausted iterator reference doesn't sit on the
          // closure until the ReadableStream is GC'd.
          it = null;
          controller.close();
        } else if (value) {
          controller.enqueue(value);
        }
      } catch (e) {
        controller.error(e);
        // Without this, the source async generator (e.g. the sync
        // RPC's bus-subscribing loop in router.ts) stays suspended at
        // `await new Promise<void>` forever. Its finally block running
        // unsubs[] never fires → sessionBus / presenceBus / etc.
        // subscribers leak per failed stream.
        const closed = it;
        it = null;
        try { await closed?.return?.(); } catch { /* ignore */ }
      }
    },
    async cancel() {
      const closed = it;
      it = null;
      if (closed?.return) await closed.return();
    },
  });
}

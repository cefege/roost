import { test, expect } from "bun:test";
import type { ConnectRouter } from "@connectrpc/connect";
import type {
  UniversalHandler,
  UniversalServerRequest,
  UniversalServerResponse,
} from "@connectrpc/connect/protocol";
import { makeConnectBunHandler } from "../src/connect/bun-handler.ts";

test("Connect request cancellation stays detached from Bun and cleans up the response stream", async () => {
  let handlerSignal: AbortSignal | undefined;
  let iteratorReturned = false;

  const handler = Object.assign(
    async (request: UniversalServerRequest): Promise<UniversalServerResponse> => {
      handlerSignal = request.signal;
      const body: AsyncIterable<Uint8Array> = {
        [Symbol.asyncIterator]() {
          return {
            next: () => new Promise<IteratorResult<Uint8Array>>((resolve) => {
              const done = () => resolve({ value: undefined, done: true });
              if (request.signal.aborted) done();
              else request.signal.addEventListener("abort", done, { once: true });
            }),
            return: async () => {
              iteratorReturned = true;
              return { value: undefined, done: true };
            },
          };
        },
      };
      return { status: 200, header: new Headers(), body };
    },
    {
      requestPath: "/test.Cancel",
      allowedMethods: ["POST"],
      method: { methodKind: "server_streaming" },
    },
  ) as UniversalHandler;
  const adapter = makeConnectBunHandler({ handlers: [handler] } as unknown as ConnectRouter);
  const bunRequestAbort = new AbortController();

  const response = await adapter.fetch(new Request("http://localhost/test.Cancel", {
    method: "POST",
    signal: bunRequestAbort.signal,
  }));
  expect(handlerSignal).toBeDefined();
  expect(handlerSignal).not.toBe(bunRequestAbort.signal);
  expect(handlerSignal!.aborted).toBe(false);

  const reader = response.body!.getReader();
  const pendingRead = reader.read();
  await reader.cancel();
  await pendingRead;

  expect(handlerSignal!.aborted).toBe(true);
  expect(iteratorReturned).toBe(true);
});

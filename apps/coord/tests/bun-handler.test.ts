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

test("unary Connect responses are buffered before the detached signal is aborted", async () => {
  let handlerSignal: AbortSignal | undefined;
  const handler = Object.assign(
    async (request: UniversalServerRequest): Promise<UniversalServerResponse> => {
      handlerSignal = request.signal;
      const body = (async function* () {
        yield new Uint8Array([1, 2]);
        yield new Uint8Array([3]);
      })();
      return { status: 200, header: new Headers(), body };
    },
    {
      requestPath: "/test.Unary",
      allowedMethods: ["POST"],
      method: { methodKind: "unary" },
    },
  ) as UniversalHandler;
  const adapter = makeConnectBunHandler({ handlers: [handler] } as unknown as ConnectRouter);
  const bunRequestAbort = new AbortController();

  const response = await adapter.fetch(new Request("http://localhost/test.Unary", {
    method: "POST",
    signal: bunRequestAbort.signal,
  }));

  expect(handlerSignal).toBeDefined();
  expect(handlerSignal).not.toBe(bunRequestAbort.signal);
  expect(handlerSignal!.aborted).toBe(true);
  expect(new Uint8Array(await response.arrayBuffer())).toEqual(new Uint8Array([1, 2, 3]));
});

test("unary body failures abort the detached handler signal", async () => {
  let handlerSignal: AbortSignal | undefined;
  const handler = Object.assign(
    async (request: UniversalServerRequest): Promise<UniversalServerResponse> => {
      handlerSignal = request.signal;
      const body = (async function* () {
        yield new Uint8Array([1]);
        throw new Error("body failed");
      })();
      return { status: 200, header: new Headers(), body };
    },
    {
      requestPath: "/test.UnaryBodyFailure",
      allowedMethods: ["POST"],
      method: { methodKind: "unary" },
    },
  ) as UniversalHandler;
  const adapter = makeConnectBunHandler({ handlers: [handler] } as unknown as ConnectRouter);

  await expect(adapter.fetch(new Request("http://localhost/test.UnaryBodyFailure", {
    method: "POST",
  }))).rejects.toThrow("body failed");
  expect(handlerSignal).toBeDefined();
  expect(handlerSignal!.aborted).toBe(true);
});

test("handler failures abort the detached handler signal", async () => {
  let handlerSignal: AbortSignal | undefined;
  const handler = Object.assign(
    async (request: UniversalServerRequest): Promise<UniversalServerResponse> => {
      handlerSignal = request.signal;
      throw new Error("handler failed");
    },
    {
      requestPath: "/test.HandlerFailure",
      allowedMethods: ["POST"],
      method: { methodKind: "unary" },
    },
  ) as UniversalHandler;
  const adapter = makeConnectBunHandler({ handlers: [handler] } as unknown as ConnectRouter);

  await expect(adapter.fetch(new Request("http://localhost/test.HandlerFailure", {
    method: "POST",
  }))).rejects.toThrow("handler failed");
  expect(handlerSignal).toBeDefined();
  expect(handlerSignal!.aborted).toBe(true);
});

test("stream response setup failures abort the detached handler signal", async () => {
  let handlerSignal: AbortSignal | undefined;
  const handler = Object.assign(
    async (request: UniversalServerRequest): Promise<UniversalServerResponse> => {
      handlerSignal = request.signal;
      const body = (async function* () {
        yield new Uint8Array([1]);
      })();
      return { status: 99, header: new Headers(), body };
    },
    {
      requestPath: "/test.ResponseFailure",
      allowedMethods: ["POST"],
      method: { methodKind: "server_streaming" },
    },
  ) as UniversalHandler;
  const adapter = makeConnectBunHandler({ handlers: [handler] } as unknown as ConnectRouter);

  await expect(adapter.fetch(new Request("http://localhost/test.ResponseFailure", {
    method: "POST",
  }))).rejects.toThrow();
  expect(handlerSignal).toBeDefined();
  expect(handlerSignal!.aborted).toBe(true);
});

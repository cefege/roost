// The relay is the only path a browser has to a machine's Mecatl daemon, and
// it answers exclusively in frames: a refusal that never reaches the
// coordinator strands the pane until its 90-second cancel. These pin the
// observable frame sequence for a normal reply, a streamed one, every refusal
// the worker owns, and cancellation.

import { afterEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { DMecatlRelayRequestSchema } from "@roost/shared/proto/worker_transport_pb";
import type { MecatlRelayChunkFrame, TransportSendResult } from "../src/transport/coord-link-types.ts";
import type { MecatlDaemon, MecatlDaemonState, MecatlLocalRequest } from "../src/mecatl/daemon.ts";
import { MecatlUnavailableError } from "../src/mecatl/daemon.ts";
import { createMecatlRelay, type MecatlRelay } from "../src/mecatl/relay.ts";

interface Harness {
  relay: MecatlRelay;
  frames: MecatlRelayChunkFrame[];
  /** Requests the daemon actually received, in arrival order. */
  seen: MecatlLocalRequest[];
  aborted: string[];
}

const servers: ReturnType<typeof Bun.serve>[] = [];
const relays: MecatlRelay[] = [];

afterEach(() => {
  while (relays.length > 0) relays.pop()!.dispose();
  while (servers.length > 0) servers.pop()!.stop(true);
});

function startUpstream(handler: (req: Request) => Response | Promise<Response>): string {
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: handler });
  servers.push(server);
  return `http://127.0.0.1:${server.port}`;
}

function harnessFor(options: {
  state?: MecatlDaemonState;
  baseUrl?: string;
  send?: (chunk: MecatlRelayChunkFrame) => TransportSendResult;
}): Harness {
  const state: MecatlDaemonState = options.state
    ?? { kind: "ready", baseUrl: options.baseUrl ?? "http://127.0.0.1:1", pid: 1 };
  const frames: MecatlRelayChunkFrame[] = [];
  const seen: MecatlLocalRequest[] = [];
  const aborted: string[] = [];
  const daemon: MecatlDaemon = {
    state: () => state,
    request: async (init) => {
      seen.push(init);
      if (state.kind !== "ready") throw new MecatlUnavailableError("daemon_exit");
      init.signal?.addEventListener("abort", () => aborted.push(init.path));
      return await fetch(`${state.baseUrl}${init.path}`, {
        method: init.method,
        headers: init.headers,
        body: init.body ? (init.body.slice().buffer as ArrayBuffer) : undefined,
        signal: init.signal,
      });
    },
    stop: async () => {},
  };
  const relay = createMecatlRelay({
    daemon,
    send: (chunk) => {
      // Copy the body: the relay hands out a subarray of the reader's buffer,
      // which the next read may reuse.
      frames.push(chunk.body ? { ...chunk, body: new Uint8Array(chunk.body) } : { ...chunk });
      return options.send?.(chunk) ?? "sent";
    },
  });
  relays.push(relay);
  return { relay, frames, seen, aborted };
}

function requestFrame(fields: {
  requestId?: string;
  method?: string;
  path?: string;
  headersJson?: string;
  body?: Uint8Array;
}) {
  return create(DMecatlRelayRequestSchema, {
    requestId: fields.requestId ?? "req-1",
    method: fields.method ?? "GET",
    path: fields.path ?? "/v1/sessions",
    headersJson: fields.headersJson ?? "{}",
    body: fields.body ?? new Uint8Array(),
  });
}

async function settled(
  harness: Harness,
  requestId: string,
  timeoutMs = 5_000,
): Promise<MecatlRelayChunkFrame[]> {
  // The exchange is done when its `end` frame lands; polling the frame log is
  // the same signal the coordinator waits on, not a guessed duration.
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const own = harness.frames.filter((frame) => frame.request_id === requestId);
    if (own.some((frame) => frame.end)) return own;
    await Bun.sleep(5);
  }
  return harness.frames.filter((frame) => frame.request_id === requestId);
}

function bodyText(frames: readonly MecatlRelayChunkFrame[]): string {
  const parts = frames.filter((frame) => frame.body).map((frame) => frame.body!);
  const total = parts.reduce((sum, part) => sum + part.byteLength, 0);
  const joined = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.byteLength;
  }
  return new TextDecoder().decode(joined);
}

test("a normal reply arrives as head, body, then exactly one end", async () => {
  const baseUrl = startUpstream(() => Response.json({ sessions: [] }));
  const harness = harnessFor({ baseUrl });

  harness.relay.handleRequest(requestFrame({}));
  const frames = await settled(harness, "req-1");

  expect(frames[0]?.head).toBe(true);
  expect(frames[0]?.status).toBe(200);
  expect(JSON.parse(frames[0]?.headers_json ?? "{}")["content-type"]).toContain("application/json");
  expect(bodyText(frames)).toBe(JSON.stringify({ sessions: [] }));
  expect(frames.filter((frame) => frame.end)).toHaveLength(1);
  expect(frames.at(-1)?.error).toBeUndefined();
});

test("an SSE reply streams as multiple body frames before its end", async () => {
  const baseUrl = startUpstream(() => {
    const stream = new ReadableStream({
      async start(controller) {
        for (let index = 0; index < 3; index += 1) {
          controller.enqueue(new TextEncoder().encode(`data: {"kind":"message.delta","n":${index}}\n\n`));
          await Bun.sleep(5);
        }
        controller.close();
      },
    });
    return new Response(stream, { headers: { "content-type": "text/event-stream" } });
  });
  const harness = harnessFor({ baseUrl });

  harness.relay.handleRequest(requestFrame({ method: "POST", path: "/v1/sessions/s1/prompt" }));
  const frames = await settled(harness, "req-1");

  expect(frames[0]?.head).toBe(true);
  expect(frames.filter((frame) => frame.body).length).toBeGreaterThan(1);
  expect(bodyText(frames)).toContain('"kind":"message.delta","n":2');
  expect(frames.at(-1)?.end).toBe(true);
  expect(frames.at(-1)?.error).toBeUndefined();
});

test("the ninth concurrent exchange is refused without reaching the daemon", async () => {
  const release = Promise.withResolvers<void>();
  const baseUrl = startUpstream(async () => {
    await release.promise;
    return new Response("done");
  });
  const harness = harnessFor({ baseUrl });

  for (let index = 0; index < 8; index += 1) {
    harness.relay.handleRequest(requestFrame({ requestId: `hold-${index}` }));
  }
  harness.relay.handleRequest(requestFrame({ requestId: "ninth" }));

  const refused = await settled(harness, "ninth");
  expect(refused).toHaveLength(1);
  expect(refused[0]?.end).toBe(true);
  expect(refused[0]?.error).toBe("relay_busy");
  expect(refused[0]?.head).toBeUndefined();
  expect(harness.seen.some((request) => request.path === "/v1/sessions" && harness.seen.length > 8)).toBe(false);

  release.resolve();
});

test("a path outside the agent API is refused before any request is made", async () => {
  const harness = harnessFor({});

  harness.relay.handleRequest(requestFrame({ path: "/internal/admin" }));
  const frames = await settled(harness, "req-1");

  expect(frames).toEqual([{ request_id: "req-1", end: true, error: "bad_path" }]);
  expect(harness.seen).toHaveLength(0);
});

test("a method the agent API never uses is refused", async () => {
  const harness = harnessFor({});

  harness.relay.handleRequest(requestFrame({ method: "PUT" }));
  const frames = await settled(harness, "req-1");

  expect(frames[0]?.error).toBe("bad_method");
  expect(harness.seen).toHaveLength(0);
});

test("an oversized request body is refused instead of forwarded", async () => {
  const harness = harnessFor({});

  harness.relay.handleRequest(requestFrame({
    method: "POST",
    body: new Uint8Array(1024 * 1024 + 1),
  }));
  const frames = await settled(harness, "req-1");

  expect(frames[0]?.error).toBe("body_too_large");
  expect(harness.seen).toHaveLength(0);
});

test("a machine whose daemon is not ready answers with that exact reason", async () => {
  const harness = harnessFor({ state: { kind: "unavailable", reason: "binary_missing" } });

  harness.relay.handleRequest(requestFrame({}));
  const frames = await settled(harness, "req-1");

  expect(frames).toEqual([{ request_id: "req-1", end: true, error: "binary_missing" }]);
  expect(harness.seen).toHaveLength(0);
});

test("an opted-out machine says disabled rather than looking crashed", async () => {
  const harness = harnessFor({ state: { kind: "disabled" } });

  harness.relay.handleRequest(requestFrame({}));

  expect(await settled(harness, "req-1")).toEqual([
    { request_id: "req-1", end: true, error: "disabled" },
  ]);
});

test("cancelling aborts the upstream request and emits no further frame", async () => {
  const started = Promise.withResolvers<void>();
  const baseUrl = startUpstream(async (req) => {
    started.resolve();
    // Held open until the relay's abort propagates.
    const aborted = Promise.withResolvers<never>();
    req.signal.addEventListener("abort", () => aborted.reject(new Error("aborted")));
    await aborted.promise;
    return new Response("unreachable");
  });
  const harness = harnessFor({ baseUrl });

  harness.relay.handleRequest(requestFrame({}));
  await started.promise;
  harness.relay.handleCancel("req-1");
  await Bun.sleep(50);

  expect(harness.aborted).toContain("/v1/sessions");
  expect(harness.frames.filter((frame) => frame.end)).toHaveLength(0);
});

test("a dropped transport ends the exchange without writing another frame", async () => {
  const baseUrl = startUpstream(() => new Response("body"));
  const harness = harnessFor({ baseUrl, send: () => "dropped" });

  harness.relay.handleRequest(requestFrame({}));
  await Bun.sleep(100);

  // The head frame was attempted and reported dropped; nothing follows it,
  // because the transport that would carry a refusal is the thing that failed.
  expect(harness.frames.filter((frame) => frame.end)).toHaveLength(0);
});

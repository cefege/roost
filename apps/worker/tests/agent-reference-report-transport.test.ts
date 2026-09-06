// Verifies the installed OMP reference transport's one-attempt ACK contract.
// Explicit rejection is distinct from a missing or malformed response after
// write, because an ambiguous durable append must never be retried.
import { expect, test } from "bun:test";
import net from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { reportAgentReference } from "../src/agent-status/report-transport.ts";

const config = (endpoint: string) => ({
  endpoint,
  capability: "a".repeat(64),
  sessionId: "11111111-1111-4111-8111-111111111111",
});

async function observeAttempt(
  response: string | null,
): Promise<{ endpoint: string; attempts: () => number; close(): Promise<void> }> {
  const root = await mkdtemp(join(tmpdir(), "roost-reference-transport-"));
  const endpoint = join(root, "agent.sock");
  let attemptCount = 0;
  const server = net.createServer((socket) => {
    socket.once("data", () => {
      attemptCount++;
      if (response !== null) socket.end(response);
    });
  });
  const listening = Promise.withResolvers<void>();
  server.listen(endpoint, listening.resolve);
  await listening.promise;
  return {
    endpoint,
    attempts: () => attemptCount,
    async close(): Promise<void> {
      const closed = Promise.withResolvers<void>();
      server.close(() => closed.resolve());
      await closed.promise;
      await rm(root, { recursive: true, force: true });
    },
  };
}

test("reference transport requires one complete explicit acknowledgement", async () => {
  if (process.platform === "win32") return;

  const rejected = await observeAttempt('{"ok":false,"error":"denied"}\n');
  expect(await reportAgentReference(
    config(rejected.endpoint),
    { kind: "id", value: "opaque" },
    100,
  )).toEqual({ status: "rejected", error: "denied" });
  expect(rejected.attempts()).toBe(1);
  await rejected.close();

  const malformed = await observeAttempt('{"ok":true,"extra":1}\n');
  expect(await reportAgentReference(
    config(malformed.endpoint),
    { kind: "path", value: "/opaque/path" },
    100,
  )).toEqual({ status: "ambiguous", reason: "invalid_response" });
  expect(malformed.attempts()).toBe(1);
  await malformed.close();

  const missing = await observeAttempt(null);
  expect(await reportAgentReference(config(missing.endpoint), null, 20)).toEqual({
    status: "ambiguous",
    reason: "timeout",
  });
  expect(missing.attempts()).toBe(1);
  await missing.close();

  const acknowledged = await observeAttempt('{"ok":true}\n');
  expect(await reportAgentReference(
    config(acknowledged.endpoint),
    { kind: "id", value: "opaque" },
    100,
  )).toEqual({ status: "acknowledged" });
  expect(acknowledged.attempts()).toBe(1);
  await acknowledged.close();
});

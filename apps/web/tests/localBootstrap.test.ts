// Pins the worker-served bootstrap contract: a page served by the worker's
// loopback UI learns which coordinator to dial, and every unusable answer
// leaves the SPA on its normal coordinator path instead of retargeting RPCs at
// the worker that served the page.

import { describe, expect, test } from "bun:test";
import { loadLocalBootstrap, readLocalBootstrap } from "../src/lib/localBootstrap.ts";
import { coordBase } from "../src/connect.ts";

interface FetchStub {
  readonly requests: string[];
  restore(): void;
}

function stubBootstrapFetch(respond: () => Response | Promise<Response>): FetchStub {
  const requests: string[] = [];
  const previous = globalThis.fetch;
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    requests.push(typeof input === "string" ? input : String(input));
    return await respond();
  }) as typeof globalThis.fetch;
  return {
    requests,
    restore() { globalThis.fetch = previous; },
  };
}

async function loadWith(respond: () => Response | Promise<Response>): Promise<string[]> {
  const stub = stubBootstrapFetch(respond);
  try {
    await loadLocalBootstrap();
    return stub.requests;
  } finally {
    stub.restore();
  }
}

function jsonResponse(body: string, status = 200): Response {
  return new Response(body, { status, headers: { "content-type": "application/json" } });
}

describe("local bootstrap", () => {
  test("a worker-served page adopts the advertised coordinator", async () => {
    const requests = await loadWith(() => jsonResponse(JSON.stringify({
      coordinatorUrl: "https://coord.example.ts.net:4102",
      workerFingerprint: "fp-local-worker",
    })));
    expect(requests).toEqual(["/api/local-bootstrap"]);
    expect(readLocalBootstrap()).toEqual({
      coordinatorUrl: "https://coord.example.ts.net:4102",
      workerFingerprint: "fp-local-worker",
    });
    expect(coordBase()).toBe("https://coord.example.ts.net:4102");
  });

  test("a coordinator-served page keeps its own origin", async () => {
    await loadWith(() => jsonResponse("Not Found", 404));
    expect(readLocalBootstrap()).toBeNull();
    expect(coordBase()).toBe("");
  });

  test("an unusable answer never retargets the SPA", async () => {
    for (const body of [
      "<!doctype html><title>spa</title>",
      "{",
      JSON.stringify({ coordinatorUrl: "https://coord.example" }),
      JSON.stringify({ workerFingerprint: "fp-local-worker" }),
      JSON.stringify({ coordinatorUrl: "  ", workerFingerprint: "fp-local-worker" }),
      JSON.stringify({ coordinatorUrl: "/api", workerFingerprint: "fp-local-worker" }),
      JSON.stringify({ coordinatorUrl: 4102, workerFingerprint: "fp-local-worker" }),
      JSON.stringify(["https://coord.example"]),
      "null",
    ]) {
      await loadWith(() => jsonResponse(body));
      expect(readLocalBootstrap()).toBeNull();
      expect(coordBase()).toBe("");
    }
  });

  test("a rejected probe is not a startup failure", async () => {
    await loadWith(() => Promise.reject(new Error("ECONNREFUSED")));
    expect(readLocalBootstrap()).toBeNull();
    expect(coordBase()).toBe("");
  });
});

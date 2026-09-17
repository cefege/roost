// Pins which worker door a page will dial: a worker-served page answers from
// its own origin without a probe, a coordinator-served page probes the default
// loopback door exactly once, an operator override redirects that probe, and
// every unusable answer leaves the page with no door at all.

import { afterAll, afterEach, beforeEach, describe, expect, test } from "bun:test";
import { loadLocalBootstrap } from "../src/lib/localBootstrap.ts";
import {
  discoverLocalWorkerDoor,
  readLocalWorkerDoor,
  registerLocalWorkerDoorHandler,
  LOCAL_WORKER_ORIGIN_KEY,
  _resetLocalWorkerDiscoveryForTest,
} from "../src/lib/localWorkerDiscovery.ts";

const PAGE_ORIGIN = "https://mic.roost.test";
const DEFAULT_DOOR_PROBE = "http://127.0.0.1:4104/api/local-bootstrap";
const DOOR_ANSWER = JSON.stringify({
  coordinatorUrl: "https://coord.other.test:4102",
  workerFingerprint: "fp-local-worker",
});

const globals = globalThis as unknown as { location: unknown; localStorage: unknown };
const priorFetch = globalThis.fetch;
const priorLocation = globals.location;
const priorLocalStorage = globals.localStorage;
const overrides = new Map<string, string>();

let requests: string[] = [];
let respond: () => Response | Promise<Response> = () => new Response(null, { status: 404 });

function jsonResponse(body: string, status = 200): Response {
  return new Response(body, { status, headers: { "content-type": "application/json" } });
}

/** The probe is fire-and-forget by design, so drain the microtask queue the
 * fetch and JSON-decode settle on rather than waiting a guessed duration. */
async function settle(): Promise<void> {
  for (let turn = 0; turn < 64; turn++) await Promise.resolve();
}

/** Puts the real localBootstrap module into its served-by-a-worker state (or
 * back to null) through its own loader, so discovery reads the same fact
 * production does. */
async function setServedByWorker(answer: string | null): Promise<void> {
  respond = () => (answer === null ? new Response(null, { status: 404 }) : jsonResponse(answer));
  await loadLocalBootstrap();
  requests = [];
}

beforeEach(async () => {
  overrides.clear();
  requests = [];
  globals.location = { origin: PAGE_ORIGIN, protocol: "https:", host: "mic.roost.test" };
  globals.localStorage = {
    getItem: (key: string) => overrides.get(key) ?? null,
    setItem: (key: string, value: string) => { overrides.set(key, value); },
    removeItem: (key: string) => { overrides.delete(key); },
  };
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    requests.push(typeof input === "string" ? input : String(input));
    return await respond();
  }) as typeof globalThis.fetch;
  await setServedByWorker(null);
  _resetLocalWorkerDiscoveryForTest();
});

afterEach(() => {
  _resetLocalWorkerDiscoveryForTest();
  globalThis.fetch = priorFetch;
});

afterAll(() => {
  globals.location = priorLocation;
  globals.localStorage = priorLocalStorage;
});

describe("local worker discovery", () => {
  test("a worker-served page adopts its own origin without probing", async () => {
    await setServedByWorker(JSON.stringify({
      coordinatorUrl: "https://coord.example:4102",
      workerFingerprint: "fp-serving-worker",
    }));

    discoverLocalWorkerDoor();
    await settle();

    expect(readLocalWorkerDoor()).toEqual({
      origin: PAGE_ORIGIN,
      workerFingerprint: "fp-serving-worker",
    });
    expect(requests).toEqual([]);
  });

  test("a coordinator-served page probes the default loopback door", async () => {
    respond = () => jsonResponse(DOOR_ANSWER);

    discoverLocalWorkerDoor();
    await settle();

    expect(requests).toEqual([DEFAULT_DOOR_PROBE]);
    expect(readLocalWorkerDoor()).toEqual({
      origin: "http://127.0.0.1:4104",
      workerFingerprint: "fp-local-worker",
    });
  });

  test("a valid override redirects the probe and a malformed one is ignored", async () => {
    respond = () => jsonResponse(DOOR_ANSWER);
    overrides.set(LOCAL_WORKER_ORIGIN_KEY, "http://127.0.0.1:9999");

    discoverLocalWorkerDoor();
    await settle();

    expect(requests).toEqual(["http://127.0.0.1:9999/api/local-bootstrap"]);
    expect(readLocalWorkerDoor()?.origin).toBe("http://127.0.0.1:9999");

    for (
      const malformed of [
        "127.0.0.1:9999",
        "http://127.0.0.1:9999/",
        "not a url",
        "ws://127.0.0.1:9999",
      ]
    ) {
      _resetLocalWorkerDiscoveryForTest();
      requests = [];
      overrides.set(LOCAL_WORKER_ORIGIN_KEY, malformed);

      discoverLocalWorkerDoor();
      await settle();

      expect(requests).toEqual([DEFAULT_DOOR_PROBE]);
    }
  });

  test("an unreachable or unusable door leaves the page with none", async () => {
    const answers: (() => Response | Promise<Response>)[] = [
      () => jsonResponse("Not Found", 404),
      () => jsonResponse("{"),
      () => jsonResponse(JSON.stringify({ coordinatorUrl: "https://coord.test" })),
      () => jsonResponse(JSON.stringify({
        coordinatorUrl: "https://coord.test",
        workerFingerprint: "  ",
      })),
      () => Promise.reject(new Error("ECONNREFUSED")),
    ];

    for (const answer of answers) {
      _resetLocalWorkerDiscoveryForTest();
      requests = [];
      respond = answer;

      discoverLocalWorkerDoor();
      await settle();

      expect(requests).toEqual([DEFAULT_DOOR_PROBE]);
      expect(readLocalWorkerDoor()).toBeNull();
    }
  });

  test("discovery runs once per page and notifies its handler on adoption", async () => {
    const adopted: string[] = [];
    registerLocalWorkerDoorHandler((found) => { adopted.push(found.workerFingerprint); });
    respond = () => jsonResponse(DOOR_ANSWER);

    discoverLocalWorkerDoor();
    await settle();
    discoverLocalWorkerDoor();
    await settle();

    expect(requests).toEqual([DEFAULT_DOOR_PROBE]);
    expect(adopted).toEqual(["fp-local-worker"]);
  });
});

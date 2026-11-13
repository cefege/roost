import { describe, expect, it } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { startProvider, type ProviderSession, type ProviderTurn } from "./provider";
import { eventually, objectValue, within } from "./runtime";

interface ProviderFixture {
  provider: ProviderSession;
  rows(): Promise<Record<string, unknown>[]>;
}

async function withProvider(turns: ProviderTurn[], exercise: (fixture: ProviderFixture) => Promise<void>): Promise<void> {
  const workspace = await mkdtemp(join(tmpdir(), "omp-provider-boundary-"));
  const tracePath = join(workspace, "provider.jsonl");
  let provider: ProviderSession | undefined;
  try {
    provider = await startProvider(turns, tracePath);
    await exercise({ provider, async rows() {
      return (await readFile(tracePath, "utf8")).split("\n").filter(Boolean)
        .map(line => objectValue(JSON.parse(line), "actual provider trace"));
    } });
  } finally {
    await provider?.stop();
    await rm(workspace, { recursive: true, force: true });
  }
}

function send(provider: ProviderSession, model: string, prompt: string, stream = false, signal?: AbortSignal): Promise<Response> {
  return fetch(`${provider.baseUrl}/chat/completions`, {
    method: "POST", headers: { "content-type": "application/json" }, signal,
    body: JSON.stringify({ model, stream, messages: [{ role: "user", content: prompt }] }),
  });
}

async function completion(response: Response): Promise<Record<string, unknown>> {
  expect(response.status).toBe(200);
  return objectValue(await response.json(), "actual completion");
}

function admittedIndices(rows: Record<string, unknown>[]): number[] {
  return rows.flatMap(row => {
    if (!row.raw || typeof row.raw !== "object") return [];
    const raw = objectValue(row.raw, "provider raw trace");
    return typeof raw.index === "number" && raw.request ? [raw.index] : [];
  });
}

describe("controlled provider HTTP dispatch", () => {
  it("serves concurrent model roles out of global tape order while preserving each model order", async () => {
    await withProvider([
      { match: { modelId: "parity-coder", textIncludes: "director first" }, text: "DIRECTOR_FIRST" },
      { match: { modelId: "parity-small", textIncludes: "worker first" }, text: "WORKER_FIRST" },
      { match: { modelId: "parity-coder", textIncludes: "director followup" }, text: "DIRECTOR_FOLLOWUP" },
    ], async fixture => {
      const worker = await completion(await send(fixture.provider, "parity-small", "worker first"));
      const director = await completion(await send(fixture.provider, "parity-coder", "director first"));
      const followup = await completion(await send(fixture.provider, "parity-coder", "director followup"));
      expect([worker.id, director.id, followup.id]).toEqual(["fixture-1", "fixture-0", "fixture-2"]);
      expect(JSON.stringify(worker.choices)).toContain("WORKER_FIRST");
      expect(JSON.stringify(director.choices)).toContain("DIRECTOR_FIRST");
      expect(JSON.stringify(followup.choices)).toContain("DIRECTOR_FOLLOWUP");
      await fixture.provider.assertConsumed();
      expect(admittedIndices(await fixture.rows())).toEqual([1, 0, 2]);
    });
  });

  it("releases held responses by their original global index, not arrival order", async () => {
    await withProvider([
      { match: { modelId: "parity-coder" }, textChunks: ["DIRECTOR_RELEASED"], chunkDelayMs: 0, holdAfterText: true },
      { match: { modelId: "parity-small" }, textChunks: ["WORKER_RELEASED"], chunkDelayMs: 0, holdAfterText: true },
    ], async fixture => {
      const worker = await send(fixture.provider, "parity-small", "worker", true);
      const director = await send(fixture.provider, "parity-coder", "director", true);
      expect(worker.status).toBe(200);
      expect(director.status).toBe(200);
      const workerText = worker.text();
      const directorText = director.text();
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("remain held");
      fixture.provider.release(0);
      const releasedDirector = await within(directorText, "director global turn 0", 1_500);
      expect(releasedDirector).toContain('"id":"fixture-0"');
      expect(releasedDirector).toContain("DIRECTOR_RELEASED");
      expect(releasedDirector).toContain("[DONE]");
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("remain held");
      fixture.provider.release(1);
      const releasedWorker = await within(workerText, "worker global turn 1", 1_500);
      expect(releasedWorker).toContain('"id":"fixture-1"');
      expect(releasedWorker).toContain("WORKER_RELEASED");
      expect(releasedWorker).toContain("[DONE]");
      expect(() => fixture.provider.release(1)).toThrow("not an active held stream");
      await fixture.provider.assertConsumed();
      expect(admittedIndices(await fixture.rows())).toEqual([1, 0]);
    });
  });

  it("rejects a wrong prompt without skipping forward or consuming the candidate", async () => {
    await withProvider([
      { match: { modelId: "parity-coder", textIncludes: "first required prompt" }, text: "FIRST" },
      { match: { modelId: "parity-coder", textIncludes: "later prompt" }, text: "LATER" },
    ], async fixture => {
      const wrong = await send(fixture.provider, "parity-coder", "later prompt");
      expect(wrong.status).toBe(409);
      expect(await wrong.text()).toContain("first required prompt");
      expect(admittedIndices(await fixture.rows())).toEqual([]);
      expect((await completion(await send(fixture.provider, "parity-coder", "first required prompt"))).id).toBe("fixture-0");
      expect((await completion(await send(fixture.provider, "parity-coder", "later prompt"))).id).toBe("fixture-1");
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("did not contain");
      expect(admittedIndices(await fixture.rows())).toEqual([0, 1]);
    });
  });

  it("admits one of two racing requests exactly once and retains the extra-request failure", async () => {
    await withProvider([{ match: { modelId: "parity-small" }, text: "ONLY_ONCE" }], async fixture => {
      const responses = await Promise.all([
        send(fixture.provider, "parity-small", "first arrival"),
        send(fixture.provider, "parity-small", "second arrival"),
      ]);
      expect(responses.map(response => response.status).sort()).toEqual([200, 409]);
      const accepted = responses.find(response => response.status === 200);
      const rejected = responses.find(response => response.status === 409);
      if (!accepted || !rejected) throw new Error("Expected one actual accepted request and one rejected request");
      expect((await completion(accepted)).id).toBe("fixture-0");
      expect(await rejected.text()).toContain("Unscripted provider request");
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("Unscripted provider request");
      expect(admittedIndices(await fixture.rows())).toEqual([0]);
    });
  });

  it("keeps sequential wildcard turns ordered without inferring purpose from the model", async () => {
    await withProvider([
      { match: { purpose: "compaction", textIncludes: "first" }, text: "FIRST_WILDCARD" },
      { match: { purpose: "foreground", textIncludes: "second" }, text: "SECOND_WILDCARD" },
    ], async fixture => {
      const first = await completion(await send(fixture.provider, "parity-small", "first"));
      const second = await completion(await send(fixture.provider, "parity-coder", "second"));
      expect([first.id, second.id]).toEqual(["fixture-0", "fixture-1"]);
      await fixture.provider.assertConsumed();
    });
  });

  it("retains a missing role obligation and rejects an unexpected route", async () => {
    await withProvider([
      { match: { modelId: "parity-coder" }, text: "DIRECTOR" },
      { match: { modelId: "parity-small" }, text: "WORKER" },
    ], async fixture => {
      await completion(await send(fixture.provider, "parity-small", "worker"));
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("missing indices: 0");
      const response = await fetch(`${fixture.provider.baseUrl}/responses`);
      expect(response.status).toBe(409);
      await response.text();
      await completion(await send(fixture.provider, "parity-coder", "director"));
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("Unexpected provider route");
    });
  });

  it("cancels a held HTTP stream at its selected global index without replaying it", async () => {
    await withProvider([
      { match: { modelId: "parity-coder" }, text: "DIRECTOR" },
      { match: { modelId: "parity-small" }, textChunks: ["WORKER_STARTED"], chunkDelayMs: 0, holdAfterText: true },
    ], async fixture => {
      const abort = new AbortController();
      const response = await send(fixture.provider, "parity-small", "worker", true, abort.signal);
      const reader = response.body?.getReader();
      if (!reader) throw new Error("Expected an actual streaming HTTP response");
      expect((await reader.read()).done).toBe(false);
      abort.abort();
      await reader.cancel().catch(() => {});
      await eventually("actual HTTP cancellation", async () => {
        const found = (await fixture.rows()).some(row => {
          if (!row.raw || typeof row.raw !== "object") return false;
          const raw = objectValue(row.raw, "cancellation trace");
          return raw.index === 1 && raw.cancelled === true;
        });
        return found ? true : undefined;
      }, 1_500);
      await completion(await send(fixture.provider, "parity-coder", "director"));
      await fixture.provider.assertConsumed();
      expect(admittedIndices(await fixture.rows())).toEqual([1, 0]);
      expect(() => fixture.provider.release(1)).toThrow("not an active held stream");
    });
  });
});

describe("controlled provider admission validation", () => {
  it("rejects an unscripted model without consuming another role's response", async () => {
    await withProvider([{ match: { modelId: "parity-coder" }, text: "DIRECTOR_ONLY" }], async fixture => {
      const unknown = await send(fixture.provider, "parity-small", "same prompt");
      expect(unknown.status).toBe(409);
      expect(await unknown.text()).toContain("Unscripted provider request");
      expect(admittedIndices(await fixture.rows())).toEqual([]);
      const actual = await completion(await send(fixture.provider, "parity-coder", "same prompt"));
      expect(actual.id).toBe("fixture-0");
      expect(JSON.stringify(actual.choices)).toContain("DIRECTOR_ONLY");
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("parity-small");
    });
  });

  it("rejects a suffix-lookalike route without consuming the real endpoint's turn", async () => {
    await withProvider([{ match: {}, text: "CANONICAL_ROUTE" }], async fixture => {
      const wrong = await fetch(`${new URL(fixture.provider.baseUrl).origin}/wrong/chat/completions`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ model: "parity-coder", messages: [{ role: "user", content: "prompt" }] }),
      });
      expect(wrong.status).toBe(409);
      await wrong.text();
      expect(admittedIndices(await fixture.rows())).toEqual([]);
      const actual = await completion(await send(fixture.provider, "parity-coder", "prompt"));
      expect(actual.id).toBe("fixture-0");
      await expect(fixture.provider.assertConsumed()).rejects.toThrow("Unexpected provider route");
    });
  });
});

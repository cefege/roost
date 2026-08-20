import { expect, test, vi } from "bun:test";

interface ViewportAdmission {
  result: Promise<unknown>;
}

export interface ViewportRetryHarness {
  claim(): ViewportAdmission;
  sequences(): bigint[];
  fail(
    kind: "viewportRejected" | "viewportAmbiguous",
    sequence: bigint,
    reason: string,
    sequenceFloor?: bigint,
  ): void;
  accept(sequence: bigint): void;
  snapshot(): unknown;
  sequenceFloor(): string;
  persistedSequence(): string | undefined;
}

export function registerViewportRetryCases(harness: ViewportRetryHarness): void {
  test("retries a rejection without an authoritative floor after the first bounded delay", async () => {
    const admission = harness.claim();
    const firstSequence = harness.sequences()[0]!;
    harness.fail("viewportRejected", firstSequence, "keeper rejected resize");
    vi.advanceTimersByTime(249);
    expect(harness.sequences()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    const retrySequence = harness.sequences()[1]!;
    expect(retrySequence).toBeGreaterThan(firstSequence);
    harness.accept(retrySequence);
    expect(await admission.result).toMatchObject({ status: "accepted", sequence: retrySequence });
  });

  test("rebases an authoritative rejection floor and redispatches floor plus one immediately", async () => {
    const admission = harness.claim();
    const firstSequence = harness.sequences()[0]!;
    const authoritativeFloor = firstSequence + 10_000n;
    harness.fail(
      "viewportRejected",
      firstSequence,
      "stale or conflicting viewport intent",
      authoritativeFloor,
    );
    expect(harness.sequences()).toHaveLength(1);
    expect(harness.snapshot()).toMatchObject({
      sequence_floor: authoritativeFloor.toString(),
      desired: { client_seq: firstSequence.toString() },
      retry: { reason: "viewport rejected: stale or conflicting viewport intent" },
    });
    expect(harness.persistedSequence()).toBe(authoritativeFloor.toString());

    vi.advanceTimersByTime(0);
    const rebasedSequence = harness.sequences()[1]!;
    expect(rebasedSequence).toBe(authoritativeFloor + 1n);
    expect(harness.sequenceFloor()).toBe(rebasedSequence.toString());
    expect(harness.persistedSequence()).toBe(rebasedSequence.toString());

    harness.fail("viewportRejected", rebasedSequence, "keeper rejected resize");
    vi.advanceTimersByTime(249);
    expect(harness.sequences()).toHaveLength(2);
    vi.advanceTimersByTime(1);
    const legacyRetrySequence = harness.sequences()[2]!;
    harness.accept(legacyRetrySequence);
    expect(await admission.result).toMatchObject({
      status: "accepted",
      sequence: legacyRetrySequence,
    });
  });

  test("retries an equal rejection floor immediately only once per desired claim", async () => {
    const admission = harness.claim();
    const firstSequence = harness.sequences()[0]!;
    harness.fail(
      "viewportRejected",
      firstSequence,
      "conflicting viewport intent",
      firstSequence,
    );
    expect(harness.sequences()).toHaveLength(1);
    vi.advanceTimersByTime(0);
    const immediateRetrySequence = harness.sequences()[1]!;
    expect(immediateRetrySequence).toBe(firstSequence + 1n);

    harness.fail(
      "viewportRejected",
      immediateRetrySequence,
      "conflicting viewport intent",
      immediateRetrySequence,
    );
    vi.advanceTimersByTime(249);
    expect(harness.sequences()).toHaveLength(2);
    vi.advanceTimersByTime(1);
    const delayedRetrySequence = harness.sequences()[2]!;
    expect(delayedRetrySequence).toBe(immediateRetrySequence + 1n);
    harness.accept(delayedRetrySequence);
    expect(await admission.result).toMatchObject({
      status: "accepted",
      sequence: delayedRetrySequence,
    });
  });

  test("keeps the bounded backoff when a rejection floor is lower than the attempted sequence", async () => {
    const admission = harness.claim();
    const firstSequence = harness.sequences()[0]!;
    expect(firstSequence).toBeGreaterThan(0n);
    harness.fail(
      "viewportRejected",
      firstSequence,
      "stale viewport intent",
      firstSequence - 1n,
    );
    vi.advanceTimersByTime(249);
    expect(harness.sequences()).toHaveLength(1);
    vi.advanceTimersByTime(1);
    const retrySequence = harness.sequences()[1]!;
    expect(retrySequence).toBe(firstSequence + 1n);
    harness.accept(retrySequence);
    expect(await admission.result).toMatchObject({ status: "accepted", sequence: retrySequence });
  });

  test("retries ambiguous outcomes forever with capped backoff", async () => {
    const admission = harness.claim();
    const delays = [250, 500, 1_000, 2_000, 2_000];
    let sequence = harness.sequences()[0]!;
    for (const delay of delays) {
      const commandCount = harness.sequences().length;
      harness.fail("viewportAmbiguous", sequence, "worker result unknown");
      vi.advanceTimersByTime(delay - 1);
      expect(harness.sequences()).toHaveLength(commandCount);
      vi.advanceTimersByTime(1);
      expect(harness.sequences()).toHaveLength(commandCount + 1);
      sequence = harness.sequences().at(-1)!;
    }
    harness.accept(sequence);
    expect(await admission.result).toMatchObject({ status: "accepted", sequence });
  });
}

import { describe, expect, test } from "bun:test";

import { describeInputOutcome } from "../../../src/client/input/terminalInputStatus.ts";

describe("composer input outcome status", () => {
  test("an accepted batch says nothing and keeps the composer empty", () => {
    expect(describeInputOutcome({ status: "accepted", inputSeq: 1n, writtenBytes: 3 })).toEqual({
      message: null,
      restoreDraft: false,
    });
  });
  test("a rejection names its reason and restores the draft", () => {
    const status = describeInputOutcome({
      status: "rejected",
      inputSeq: 1n,
      writtenBytes: 0,
      reason: "terminal domain is resubscribing; input was not sent",
    });
    expect(status.message).toBe(
      "Not sent — terminal domain is resubscribing; input was not sent",
    );
    expect(status.restoreDraft).toBe(true);
  });
  test("an unconfirmed batch with no written bytes never claims a partial send", () => {
    const status = describeInputOutcome({
      status: "ambiguous",
      inputSeq: 1n,
      writtenBytes: 0,
      reason: "timeout",
    });
    expect(status.message).toContain("Delivery unconfirmed — timeout");
    expect(status.message?.toLowerCase()).not.toContain("partially");
    expect(status.restoreDraft).toBe(true);
  });
  test("an unconfirmed batch with written bytes reports the byte count", () => {
    const status = describeInputOutcome({
      status: "ambiguous",
      inputSeq: 1n,
      writtenBytes: 3,
      reason: "keeper acknowledged an incomplete input batch",
    });
    expect(status.message).toContain("Partially sent (3 bytes)");
    expect(status.message).toContain("keeper acknowledged an incomplete input batch");
    expect(status.restoreDraft).toBe(true);
  });
  test("a multi-line oversized reason becomes one bounded line", () => {
    const status = describeInputOutcome({
      status: "ambiguous",
      inputSeq: 1n,
      writtenBytes: 0,
      reason: `worker refused\n  the write\n${"x".repeat(400)}`,
    });
    const message = status.message ?? "";
    expect(message).not.toContain("\n");
    const reason = message.slice("Delivery unconfirmed — ".length).split(". Nothing")[0] ?? "";
    expect(reason.length).toBeLessThanOrEqual(160);
    expect(reason.startsWith("worker refused the write")).toBe(true);
  });
  test("a blank reason still says something", () => {
    expect(
      describeInputOutcome({ status: "rejected", inputSeq: 1n, writtenBytes: 0, reason: "   " })
        .message,
    ).toBe("Not sent — no reason reported");
  });
});

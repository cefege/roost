import { describe, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";

describe("PasswordWorkGate", () => {
  test("runs two operations, queues four, and rejects the seventh before work", async () => {
    const gate = new PasswordWorkGate();
    const release = Promise.withResolvers<void>();
    let active = 0;
    let maxActive = 0;
    let started = 0;
    let rejectedWorkStarted = false;

    const operations = Array.from({ length: 6 }, (_, index) => gate.run(async () => {
      started++;
      active++;
      maxActive = Math.max(maxActive, active);
      try {
        await release.promise;
        return index;
      } finally {
        active--;
      }
    }));

    expect(gate.running).toBe(2);
    expect(gate.queued).toBe(4);

    const rejected = gate.run(async () => {
      rejectedWorkStarted = true;
      return 6;
    });
    await expect(rejected).rejects.toMatchObject({ code: Code.ResourceExhausted });
    try {
      await rejected;
    } catch (error) {
      expect(error).toBeInstanceOf(ConnectError);
      expect((error as ConnectError).metadata.get("retry-after")).toBe("60");
    }
    expect(rejectedWorkStarted).toBe(false);

    release.resolve();
    await expect(Promise.all(operations)).resolves.toEqual([0, 1, 2, 3, 4, 5]);
    expect(started).toBe(6);
    expect(maxActive).toBe(2);
    expect(gate.running).toBe(0);
    expect(gate.queued).toBe(0);
  });

  test("one hundred concurrent attempts never exceed two running plus four queued", async () => {
    const gate = new PasswordWorkGate();
    const release = Promise.withResolvers<void>();
    let active = 0;
    let maxActive = 0;
    let started = 0;
    const attempts = Array.from({ length: 100 }, () =>
      gate.run(async () => {
        started++;
        active++;
        maxActive = Math.max(maxActive, active);
        try {
          await release.promise;
          return "accepted" as const;
        } finally {
          active--;
        }
      }).catch(() => "rejected" as const)
    );
    expect(gate.running).toBe(2);
    expect(gate.queued).toBe(4);
    release.resolve();
    const results = await Promise.all(attempts);
    expect(results.filter((result) => result === "accepted")).toHaveLength(6);
    expect(results.filter((result) => result === "rejected")).toHaveLength(94);
    expect(started).toBe(6);
    expect(maxActive).toBe(2);
  });
});

import { describe, expect, test } from "bun:test";
import { RateLimiter } from "../src/middleware/rate-limit.ts";

describe("RateLimiter", () => {
  test("never grows beyond the bucket cap and admits after expired LRU entries prune", () => {
    let now = 0;
    const rejected: Array<{ key: string; group: string; capacity: boolean }> = [];
    const limiter = new RateLimiter({
      now: () => now,
      maxBuckets: 3,
      onReject: (fields) => rejected.push(fields),
    });

    expect(limiter.consume("a", "base", 1, 100).allowed).toBe(true);
    now = 1;
    expect(limiter.consume("b", "base", 1, 100).allowed).toBe(true);
    now = 2;
    expect(limiter.consume("c", "base", 1, 100).allowed).toBe(true);
    expect(limiter.bucketCount).toBe(3);

    expect(limiter.consume("d", "base", 1, 100).allowed).toBe(false);
    expect(limiter.consume("e", "base", 1, 100).allowed).toBe(false);
    expect(limiter.bucketCount).toBe(3);
    expect(rejected).toEqual([{ key: "capacity", group: "base", capacity: true }]);

    now = 101;
    expect(limiter.consume("d", "base", 1, 100).allowed).toBe(true);
    expect(limiter.bucketCount).toBe(2);
    expect(limiter.consume("c", "base", 1, 100).allowed).toBe(false);
  });

  test("logs at most once for a rejected key in each fixed window", () => {
    let now = 10_000;
    const rejected: Array<{ key: string; group: string; capacity: boolean }> = [];
    const limiter = new RateLimiter({
      now: () => now,
      onReject: (fields) => rejected.push(fields),
    });

    expect(limiter.consume("203.0.113.7", "password", 1).allowed).toBe(true);
    expect(limiter.consume("203.0.113.7", "password", 1).allowed).toBe(false);
    expect(limiter.consume("203.0.113.7", "password", 1).allowed).toBe(false);
    expect(rejected).toEqual([{
      key: "203.0.113.7",
      group: "password",
      capacity: false,
    }]);

    now += 60_000;
    expect(limiter.consume("203.0.113.7", "password", 1).allowed).toBe(true);
    expect(limiter.consume("203.0.113.7", "password", 1).allowed).toBe(false);
    expect(rejected).toHaveLength(2);
  });
});

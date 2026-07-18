// Exhaustive unit test for the coord agent-status arbiter (the consistency fix).
// Covers the priority order + the working→idle hold that kills the
// running↔idle flap when claude pauses between tool calls.

import { test, expect, describe } from "bun:test";
import { resolveAgentStatus, type ArbiterInputs } from "../src/detect/arbiter.ts";

const base: ArbiterInputs = { prev: undefined, screenStatus: null, screenBlocker: false, recentBytes: false };

describe("coord arbiter priority", () => {
  test("visible blocker ⇒ needs-input (beats running/recent bytes)", () => {
    expect(resolveAgentStatus({ ...base, screenBlocker: true, recentBytes: true }).next).toBe("needs-input");
    expect(resolveAgentStatus({ ...base, screenStatus: "needs-input" }).next).toBe("needs-input");
  });

  test("manifest 'running' (OSC spinner) ⇒ running", () => {
    expect(resolveAgentStatus({ ...base, screenStatus: "running" }).next).toBe("running");
    // recent bytes alone (manifest no-opinion) does NOT force running — shells stay quiet
    expect(resolveAgentStatus({ ...base, screenStatus: null, recentBytes: true }).next).toBeUndefined();
  });

  test("manifest 'idle' (settled) ⇒ idle", () => {
    expect(resolveAgentStatus({ ...base, screenStatus: "idle" }).next).toBe("idle");
    expect(resolveAgentStatus({ ...base, prev: "idle", screenStatus: "idle" }).next).toBe("idle");
  });

  test("no opinion ⇒ hold previous (undefined ⇒ no publish)", () => {
    expect(resolveAgentStatus({ ...base, prev: "running", screenStatus: null }).next).toBe("running");
    expect(resolveAgentStatus({ ...base, screenStatus: null }).next).toBeUndefined();
  });
});

describe("working→idle hold (anti-flap)", () => {
  test("running→idle while bytes still recent ⇒ hold running + flag reeval", () => {
    const r = resolveAgentStatus({ ...base, prev: "running", screenStatus: "idle", recentBytes: true });
    expect(r.next).toBe("running");
    expect(r.reevalForIdle).toBe(true);
  });

  test("running→idle once quiet (no recent bytes) ⇒ idle commits, no reeval", () => {
    const r = resolveAgentStatus({ ...base, prev: "running", screenStatus: "idle", recentBytes: false });
    expect(r.next).toBe("idle");
    expect(r.reevalForIdle).toBe(false);
  });

  test("idle from non-running prev commits immediately (no hold)", () => {
    expect(resolveAgentStatus({ ...base, prev: "idle", screenStatus: "idle", recentBytes: true }).reevalForIdle).toBe(false);
  });
});

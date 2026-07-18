// lastVisited — browser-local "where was I" memory. Asserts rememberVisit
// writes both the boot-restore path (stable /t/ href) and the per-folder
// last-session map, and the getters read them back; folders key by
// (worker_fp, spawn_cwd); a session with no spawn_cwd falls back to /s/.

import { expect, test, describe, beforeEach } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";

// bun test has no localStorage — stub before importing the module under test.
const _ls: Record<string, string> = {};
(globalThis as unknown as { localStorage: Storage }).localStorage = {
  getItem: (k: string) => _ls[k] ?? null,
  setItem: (k: string, v: string) => { _ls[k] = v; },
  removeItem: (k: string) => { delete _ls[k]; },
  clear: () => { for (const k of Object.keys(_ls)) delete _ls[k]; },
  key: () => null, length: 0,
} as Storage;

const { rememberVisit, getLastTerminalPath, getLastSessionForFolder } = await import("../src/lib/lastVisited.ts");

const FP = asWorkerFp("aa".repeat(32));

function sess(over: Omit<Partial<Session>, "id"> & { id: string }): Session {
  const { id, ...rest } = over;
  return {
    worker_fp: FP, channel: asChannelId(1), kind: "shell",
    cwd: "/Users/you/roost", spawn_cwd: "/Users/you/roost",
    workspace_id: null, status: "open", agent: null,
    created_at: 1000, closed_at: null, custom_title: null,
    ...rest, id: asSessionId(id),
  } as Session;
}

describe("lastVisited", () => {
  beforeEach(() => localStorage.clear());

  test("rememberVisit stores the stable /t/ path + per-folder session", () => {
    rememberVisit(sess({ id: "00000000-0000-4000-8000-000000000001" }));
    expect(getLastTerminalPath()).toBe(`/t/${FP}/Users/you/roost`);
    expect(getLastSessionForFolder(FP, "/Users/you/roost")).toBe(
      "00000000-0000-4000-8000-000000000001",
    );
  });

  test("folder map keys by (worker, spawn folder); newest visit wins per folder", () => {
    rememberVisit(sess({ id: "00000000-0000-4000-8000-000000000001", spawn_cwd: "/a" }));
    rememberVisit(sess({ id: "00000000-0000-4000-8000-000000000002", spawn_cwd: "/b" }));
    rememberVisit(sess({ id: "00000000-0000-4000-8000-000000000003", spawn_cwd: "/a" }));
    expect(getLastSessionForFolder(FP, "/a")).toBe("00000000-0000-4000-8000-000000000003");
    expect(getLastSessionForFolder(FP, "/b")).toBe("00000000-0000-4000-8000-000000000002");
  });

  test("getters return null when nothing stored / folder unknown", () => {
    expect(getLastTerminalPath()).toBeNull();
    expect(getLastSessionForFolder(FP, "/never")).toBeNull();
  });

  test("session without spawn_cwd stores the /s/ fallback path + keys folder by cwd", () => {
    const s = sess({ id: "00000000-0000-4000-8000-000000000009", cwd: "/legacy" });
    delete (s as { spawn_cwd?: string }).spawn_cwd;
    rememberVisit(s);
    expect(getLastTerminalPath()).toBe("/s/00000000-0000-4000-8000-000000000009");
    expect(getLastSessionForFolder(FP, "/legacy")).toBe("00000000-0000-4000-8000-000000000009");
  });
});

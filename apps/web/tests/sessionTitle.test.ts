// Grapheme-safe truncation for user-visible session/OSC titles: a plain
// `.slice(0, MAX)` can split a surrogate pair or a multi-codepoint grapheme
// cluster (ZWJ family emoji) at the boundary, producing a lone surrogate
// (U+FFFD/tofu) or a broken cluster. folderHeadline/programSubtitle/sessionTitle
// must never do that, and must still enforce the MAX=80 code-unit cap.

import { expect, test, describe } from "bun:test";
import { asWorkerFp, asSessionId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { folderHeadline, programSubtitle, sessionTitle } from "../src/lib/sessionTitle.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";

const FP = asWorkerFp("a".repeat(64));
const SESSION_ID = asSessionId("00000000-0000-0000-0000-000000000001");

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: SESSION_ID,
    worker_fp: FP,
    channel: 1 as never,
    kind: "shell" as never,
    cwd: "/home/user/project",
    spawn_cwd: null as never,
    workspace_id: null as never,
    custom_title: null,
    created_at: 1000 as never,
    closed_at: null,
    exit_code: null as never,
    ...overrides,
  } as Session;
}

function hasLoneSurrogate(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const code = s.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      if (i + 1 >= s.length || s.charCodeAt(i + 1) < 0xdc00 || s.charCodeAt(i + 1) > 0xdfff) return true;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      if (i === 0 || s.charCodeAt(i - 1) < 0xd800 || s.charCodeAt(i - 1) > 0xdbff) return true;
    }
  }
  return false;
}

describe("session title truncation", () => {
  test("79 ASCII chars + one astral emoji truncates without a lone surrogate or U+FFFD", () => {
    const rocket = "\u{1F680}"; // 2 UTF-16 code units
    const title = "a".repeat(79) + rocket;
    expect(title.length).toBe(81);
    const session = makeSession({ custom_title: title });
    const result = folderHeadline(session);
    expect(result.length).toBeLessThanOrEqual(80);
    expect(hasLoneSurrogate(result)).toBe(false);
    expect(result).not.toContain("\uFFFD");
    // The emoji straddles the 79/81 boundary — it must be dropped whole,
    // not split.
    expect(result).toBe("a".repeat(79));
  });

  test("a ZWJ family emoji at the boundary is not split mid-cluster", () => {
    // Family: man+ZWJ+woman+ZWJ+girl+ZWJ+boy — one grapheme cluster, 11 code units.
    const family = "\u{1F468}\u200D\u{1F469}\u200D\u{1F467}\u200D\u{1F466}";
    const title = "b".repeat(75) + family; // 75 + 11 = 86 code units total
    const session = makeSession({ custom_title: title });
    const result = sessionTitle(session);
    expect(result.length).toBeLessThanOrEqual(80);
    expect(hasLoneSurrogate(result)).toBe(false);
    // Cluster does not fit (75 + 11 > 80), so it must be dropped whole,
    // never leaving a partial ZWJ fragment.
    expect(result).toBe("b".repeat(75));
    expect(result).not.toContain("\u200D");
  });

  test("a short title is returned unchanged", () => {
    const session = makeSession({ custom_title: "hello" });
    expect(folderHeadline(session)).toBe("hello");
    expect(sessionTitle(session)).toBe("hello");
  });

  test("the cap is still enforced for programSubtitle (OSC title)", () => {
    setRootStore("terminal_title", SESSION_ID, "c".repeat(120));
    const session = makeSession();
    const result = programSubtitle(session);
    expect(result).not.toBeNull();
    expect(result!.length).toBe(80);
    setRootStore("terminal_title", SESSION_ID, undefined as unknown as string);
    void rootStore;
  });
});

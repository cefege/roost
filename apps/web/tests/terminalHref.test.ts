// terminalHref / encodeFolderPath / decodeFolderPath — the /t/ URL builders.
// Asserts: encode↔decode round-trips (incl. spaces/unicode); terminalHref
// builds /t/<fp>/<folder>; falls back to /s/<id> when spawn_cwd is absent.

import { expect, test, describe } from "bun:test";
import { asWorkerFp, asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { terminalHref, encodeFolderPath, decodeFolderPath } from "../src/lib/terminalHref.ts";

const FP = asWorkerFp("aa".repeat(32));

function sess(over: Partial<Session>): Session {
  return {
    worker_fp: FP, channel: asChannelId(1), kind: "shell",
    cwd: "/Users/you/roost", spawn_cwd: "/Users/you/roost",
    workspace_id: null, status: "open",
    created_at: 1000, closed_at: null, custom_title: null,
    ...over,
    id: over.id ?? asSessionId("00000000-0000-4000-8000-000000000001"),
  } as Session;
}

describe("terminalHref URL builders", () => {
  test.each([
    "/Users/you/roost",
    "/Users/you/My Folder",
    "/Users/you/café/apps",
    "/",
  ])("encode↔decode round-trips: %s", (folder) => {
    expect(decodeFolderPath(encodeFolderPath(folder))).toBe(folder);
  });

  test("encode strips the leading slash + percent-encodes spaces", () => {
    expect(encodeFolderPath("/Users/you/My Folder")).toBe("Users/you/My%20Folder");
  });

  test("terminalHref builds /t/<fp>/<encoded folder>", () => {
    expect(terminalHref(sess({}))).toBe(`/t/${FP}/Users/you/roost`);
  });
  test("Windows drive and UNC folders use explicit reversible route tags", () => {
    const drive = "C:/Users/Ada/My Folder";
    const unc = "//fileserver/team/Build Artifacts";
    expect(encodeFolderPath(drive)).toBe("~drive/C/Users/Ada/My%20Folder");
    expect(decodeFolderPath(encodeFolderPath(drive))).toBe(drive);
    expect(encodeFolderPath(unc)).toBe("~unc/fileserver/team/Build%20Artifacts");
    expect(decodeFolderPath(encodeFolderPath(unc))).toBe(unc);
  });

  test("terminalHref tags a caller session's Windows drive path", () => {
    expect(terminalHref(sess({
      cwd: "D:/src/roost",
      spawn_cwd: "D:/src/roost",
    }))).toBe(`/t/${FP}/~drive/D/src/roost`);
  });

  test("falls back to /s/<id> when spawn_cwd is absent", () => {
    const s = sess({ id: asSessionId("00000000-0000-4000-8000-000000000009") });
    delete (s as { spawn_cwd?: string }).spawn_cwd;
    expect(terminalHref(s)).toBe("/s/00000000-0000-4000-8000-000000000009");
  });
});

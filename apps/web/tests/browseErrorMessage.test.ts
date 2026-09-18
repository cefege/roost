import { expect, test } from "bun:test";
import { browseErrorMessage } from "../src/lib/browseErrorMessage.ts";

test("errno text buried in a Connect message maps to filesystem copy", () => {
  expect(browseErrorMessage(new Error("[internal] mkdir: EACCES: permission denied, mkdir '/opt/x'")))
    .toBe("Permission denied on this machine.");
  expect(browseErrorMessage(new Error("EPERM: operation not permitted"))).toBe(
    "Permission denied on this machine.",
  );
  expect(browseErrorMessage(new Error("ENOSPC: no space left on device"))).toBe(
    "The machine is out of disk space.",
  );
  expect(browseErrorMessage(new Error("EROFS: read-only file system"))).toBe(
    "That location is read-only.",
  );
  expect(browseErrorMessage(new Error("EEXIST: file already exists, mkdir '/a/b'"))).toBe(
    "A file with that name already exists here.",
  );
  expect(browseErrorMessage(new Error("ENOTDIR: not a directory, mkdir '/a/file/b'"))).toBe(
    "Part of that path isn't a folder.",
  );
  expect(browseErrorMessage(new Error("ENAMETOOLONG: name too long"))).toBe(
    "That path is too long for this machine.",
  );
  expect(browseErrorMessage(new Error("ENOENT: no such file or directory"))).toBe(
    "That folder no longer exists.",
  );
});

test("transport failures map to machine-state copy", () => {
  expect(browseErrorMessage(new Error("[failed_precondition] worker not connected"))).toBe(
    "The machine went offline. Reconnect and try again.",
  );
  expect(browseErrorMessage(new Error("[unavailable] connection reset"))).toBe(
    "The machine went offline. Reconnect and try again.",
  );
  expect(browseErrorMessage(new Error("worker offline"))).toBe(
    "The machine went offline. Reconnect and try again.",
  );
  expect(browseErrorMessage(new Error("worker did not reply in time"))).toBe(
    "The machine didn't respond. Try again.",
  );
  expect(browseErrorMessage(new Error("[deadline_exceeded]"))).toBe(
    "The machine didn't respond. Try again.",
  );
  expect(browseErrorMessage(new Error("[unauthenticated] token rejected"))).toBe(
    "Your session expired. Reload to sign in again.",
  );
  expect(browseErrorMessage(new Error("authentication required"))).toBe(
    "Your session expired. Reload to sign in again.",
  );
  expect(browseErrorMessage(new Error("[not_found] worker not found"))).toBe(
    "This machine is no longer registered.",
  );
});

test("the path codec's join rejection is reported as the name rule it really is", () => {
  expect(browseErrorMessage(new Error("nativePathJoin only accepts relative child parts: a/b")))
    .toBe("Folder names can't contain / or \\.");
});

test("an unmapped message passes through verbatim, casing intact", () => {
  expect(browseErrorMessage(new Error("Something Odd Happened On The Worker"))).toBe(
    "Something Odd Happened On The Worker",
  );
});

test("non-Error values are stringified", () => {
  expect(browseErrorMessage("EACCES denied")).toBe("Permission denied on this machine.");
  expect(browseErrorMessage("PlainString")).toBe("PlainString");
  expect(browseErrorMessage(undefined)).toBe("undefined");
  expect(browseErrorMessage({ code: 7 })).toBe("[object Object]");
});

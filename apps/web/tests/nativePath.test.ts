import { describe, expect, test } from "bun:test";
import {
  resolveWorkerPath,
  sameWorkerPath,
  shortWorkerPath,
  workerFileHref,
  workerPathCrumbs,
} from "../src/lib/nativePath.ts";

describe("browser worker-native path adapter", () => {
  test("resolves Windows absolute, relative, rooted, and home paths canonically", () => {
    expect(resolveWorkerPath("win", "C:/Users/Ada/src", String.raw`D:\logs\app.log`))
      .toBe("D:/logs/app.log");
    expect(resolveWorkerPath("win", "C:/Users/Ada/src", String.raw`lib\main.ts`))
      .toBe("C:/Users/Ada/src/lib/main.ts");
    expect(resolveWorkerPath("win", "C:/Users/Ada/src", "/Windows/System32/kernel32.dll"))
      .toBe("C:/Windows/System32/kernel32.dll");
    expect(resolveWorkerPath("win", "C:/Users/Ada/src", "~/notes.txt"))
      .toBe("C:/Users/Ada/notes.txt");
    expect(resolveWorkerPath("win", "C:/Users/Ada/src", "D:drive-relative.txt")).toBeNull();
  });

  test("Windows identity comparisons case-fold without changing display", () => {
    expect(sameWorkerPath("win", "C:/Users/Ada/Code", "c:/users/ada/code")).toBe(true);
    expect(shortWorkerPath("win", "C:/Users/Ada/Code/roost")).toBe("Ada/roost");
  });

  test("UNC crumbs and file href retain the server/share root", () => {
    expect(workerPathCrumbs("win", "//Server/Share/src")[0]).toEqual({
      label: "//Server/Share",
      path: "//Server/Share",
    });
    expect(workerFileHref("win", "//Server/Share/a b.txt"))
      .toBe("/file/win/~unc/Server/Share/a%20b.txt");
  });
});

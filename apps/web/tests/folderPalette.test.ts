import { expect, test } from "bun:test";
import { childPath, pathCrumbs, parentPath } from "../src/lib/folderPalette.ts";

test("childPath joins, handling root + trailing slash", () => {
  expect(childPath("~/code", "roost")).toBe("~/code/roost");
  expect(childPath("~/code/", "roost")).toBe("~/code/roost");
  expect(childPath("/", "usr")).toBe("/usr");
});

test("pathCrumbs: cumulative segments, abs + home roots", () => {
  expect(pathCrumbs("/Users/you")).toEqual([
    { label: "/", path: "/" },
    { label: "Users", path: "/Users" },
    { label: "you", path: "/Users/you" },
  ]);
  expect(pathCrumbs("~/Code/roost")).toEqual([
    { label: "~", path: "~" },
    { label: "Code", path: "~/Code" },
    { label: "roost", path: "~/Code/roost" },
  ]);
  expect(pathCrumbs("")).toEqual([]);
});

test("parentPath drops last segment; roots stay put", () => {
  expect(parentPath("/Users/you")).toBe("/Users");
  expect(parentPath("~/Code")).toBe("~");
  expect(parentPath("~")).toBe("~");
  expect(parentPath("/")).toBe("/");
});

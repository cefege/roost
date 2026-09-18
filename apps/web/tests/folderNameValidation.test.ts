import { expect, test } from "bun:test";
import { validateNewFolderName } from "../src/lib/folderNameValidation.ts";

test("rejects path separators with the literal picker copy", () => {
  expect(validateNewFolderName("a/b", [])).toEqual({
    ok: false,
    message: "Folder names can't contain / or \\.",
  });
  expect(validateNewFolderName("a\\b", [])).toEqual({
    ok: false,
    message: "Folder names can't contain / or \\.",
  });
});

test("blank and whitespace-only names ask for a name", () => {
  expect(validateNewFolderName("", [])).toEqual({ ok: false, message: "Enter a folder name." });
  expect(validateNewFolderName("  ", [])).toEqual({ ok: false, message: "Enter a folder name." });
});

test("sibling collision is case-insensitive and quotes the trimmed name", () => {
  expect(validateNewFolderName("Docs", ["docs"])).toEqual({
    ok: false,
    message: 'A folder named "Docs" already exists here.',
  });
  expect(validateNewFolderName("  Docs  ", ["DOCS", "src"])).toEqual({
    ok: false,
    message: 'A folder named "Docs" already exists here.',
  });
});

test("dot names report the dot message, not the trailing-period message", () => {
  expect(validateNewFolderName("..", [])).toEqual({
    ok: false,
    message: "Choose a name other than . or ..",
  });
  expect(validateNewFolderName(".", [])).toEqual({
    ok: false,
    message: "Choose a name other than . or ..",
  });
});

test("control characters are named separately from separators", () => {
  expect(validateNewFolderName("na\u0007me", [])).toEqual({
    ok: false,
    message: "Folder names can't contain control characters.",
  });
  expect(validateNewFolderName("na\tme", [])).toEqual({
    ok: false,
    message: "Folder names can't contain control characters.",
  });
});

test("trailing period is rejected; trailing spaces are trimmed away", () => {
  expect(validateNewFolderName("build.", [])).toEqual({
    ok: false,
    message: "Folder names can't end with a space or period.",
  });
  expect(validateNewFolderName("docs .", [])).toEqual({
    ok: false,
    message: "Folder names can't end with a space or period.",
  });
  expect(validateNewFolderName("folder  ", [])).toEqual({ ok: true });
  expect(validateNewFolderName("my folder", [])).toEqual({ ok: true });
});

test("255 characters is allowed, 256 is not", () => {
  expect(validateNewFolderName("n".repeat(255), [])).toEqual({ ok: true });
  expect(validateNewFolderName("n".repeat(256), [])).toEqual({
    ok: false,
    message: "Folder names must be 255 characters or fewer.",
  });
});

test("a fresh name beside unrelated siblings passes", () => {
  expect(validateNewFolderName("ok", ["other"])).toEqual({ ok: true });
});

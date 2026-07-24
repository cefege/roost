// Quick-chat convention: isChatFolder path detection + newChatFolderPath shape
// and uniqueness. Pure functions, no store/RPC — matches the folderGroups test
// style (bun:test, direct calls).

import { expect, test, describe } from "bun:test";
import { isChatFolder, newChatFolderPath } from "../src/lib/quickChat.ts";

describe("isChatFolder", () => {
  test("true for a chat scratch dir", () => {
    expect(isChatFolder("/Users/x/.roost/chats/chat-20260724-120000-ab12")).toBe(true);
  });
  test("false for a real workspace", () => {
    expect(isChatFolder("/Users/x/Code/idea")).toBe(false);
  });
});

describe("newChatFolderPath", () => {
  test("matches the expected shape", () => {
    expect(newChatFolderPath()).toMatch(/^~\/\.roost\/chats\/chat-\d{8}-\d{6}-.{4}$/);
  });
  test("two consecutive calls differ", () => {
    expect(newChatFolderPath()).not.toBe(newChatFolderPath());
  });
});

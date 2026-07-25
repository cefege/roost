// Welcome-card tip projection: TIP_COMMANDS order wins over the response's,
// unlisted/description-less commands are dropped, and any malformed payload
// degrades to no tips block. Pure function, no store/RPC — matches the
// quickChat test style (bun:test, direct calls).

import { expect, test, describe } from "bun:test";
import { pickTips } from "../src/components/chat/omp/welcomeTips.ts";

describe("pickTips", () => {
  test("keeps TIP_COMMANDS order and drops unlisted names", () => {
    expect(pickTips({
      commands: [
        { name: "context", description: "Show context usage" },
        { name: "model", description: "Switch model" },
        { name: "zzz", description: "x" },
      ],
    })).toEqual([
      { name: "model", description: "Switch model" },
      { name: "context", description: "Show context usage" },
    ]);
  });

  test("skips entries with an empty or absent description", () => {
    expect(pickTips({ commands: [{ name: "model", description: "" }, { name: "context" }] })).toEqual([]);
  });

  test("returns [] for a malformed payload", () => {
    expect(pickTips(null)).toEqual([]);
    expect(pickTips({})).toEqual([]);
    expect(pickTips({ commands: "nope" })).toEqual([]);
  });
});

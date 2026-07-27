import { describe, expect, test } from "bun:test";
import { coordinatorRelocationFragment } from "../src/auth/coordinator-relocation.ts";

describe("coordinator move browser contracts", () => {
  test("requires both opaque relocation fragment fields", () => {
    expect(coordinatorRelocationFragment("#move=token&handoff=handoff-id")).toEqual({ token: "token", handoffId: "handoff-id" });
    expect(coordinatorRelocationFragment("#move=token")).toBeNull();
    expect(coordinatorRelocationFragment("#handoff=handoff-id")).toBeNull();
  });

});

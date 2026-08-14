import { describe, expect, test } from "bun:test";
import { finishWorkerDeploy } from "../src/deploy-exec.ts";

describe("worker deployment verification", () => {
  test("a failed verification preserves output and never prints success", () => {
    const logs: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => { logs.push(args.join(" ")); };
    try {
      expect(() => finishWorkerDeploy(
        { exit: 3, stdout: "service state: failed", stderr: "launchctl: not found" },
        ">> done — worker deployed",
      )).toThrow(/service state: failed[\s\S]*launchctl: not found/);
      expect(logs.join("\n")).not.toContain("done");
    } finally {
      console.log = originalLog;
    }
  });

  test("a successful verification prints state before the completion marker", () => {
    const logs: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => { logs.push(args.join(" ")); };
    try {
      finishWorkerDeploy(
        { exit: 0, stdout: "state = running\npid = 123\n", stderr: "" },
        ">> done — worker deployed",
      );
      expect(logs).toEqual(["   state = running\n   pid = 123", ">> done — worker deployed"]);
    } finally {
      console.log = originalLog;
    }
  });
});

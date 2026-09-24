import { describe, expect, test } from "bun:test";
import {
  coordServicePath,
  roostServiceDir,
  roostVersionsDir,
  windowsVersionedBinaryPath,
  workerDataDir,
} from "../src/paths.ts";

describe("Windows machine and service paths", () => {
  const env = {
    ProgramData: "C:\\ProgramData",
    LOCALAPPDATA: "C:\\Users\\operator\\AppData\\Local",
    USERPROFILE: "C:\\Users\\operator",
  };

  test("keeps executable and service control state out of the interactive profile", () => {
    expect(roostServiceDir(env, "win32")).toBe("C:\\ProgramData\\Roost\\service");
    expect(roostVersionsDir(env, "win32")).toBe("C:\\ProgramData\\Roost\\versions");
    expect(coordServicePath(env, "win32")).toBe("C:\\ProgramData\\Roost\\service\\coordinator.json");
    expect(windowsVersionedBinaryPath("2.3.4", env)).toBe("C:\\ProgramData\\Roost\\versions\\2.3.4\\roost.exe");
  });

  test("retains profile-local interactive worker data unless the service overrides it", () => {
    expect(workerDataDir(env, "win32")).toBe("C:\\Users\\operator\\AppData\\Local\\Roost\\WorkerV2");
  });
});

import { supportedHostPlatform } from "@roost/shared/platform";
import type { ShellSpec } from "../src/shell-spec.ts";

interface KeeperTestShellSpecOptions {
  executable: string;
  argv?: string[];
  cwd: string;
  env?: Record<string, string>;
}

/** Builds the complete launch contract expected at the keeper boundary. */
export function keeperTestShellSpec({
  executable,
  argv = [],
  cwd,
  env = { TERM: "xterm-256color" },
}: KeeperTestShellSpecOptions): ShellSpec {
  return {
    version: 1,
    platform: supportedHostPlatform(),
    executable,
    argv,
    cwd,
    env,
  };
}

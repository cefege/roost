// `roost hook <event>` — Claude Code hook shim. The compiled binary's self-exec
// target: claude runs `roost hook <event>` per session-constants HOOK_CMD (from
// source it's `bun run cli/hook.ts <event>`). Internal, not user-facing.
import { runHook } from "../../worker/src/cli/hook.ts";

export function hook(args: string[]): void {
  runHook(args[0] ?? "unknown");
}

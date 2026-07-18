// `roost keeper <sock>` — run the multiplexed keeper in THIS process. This is
// the compiled binary's self-exec target: the worker spawns `roost keeper
// <sock>` instead of `bun run multiplexed-main.ts <sock>` when it isn't
// running under bun (see keeper-pool-lifecycle.ts). Internal, not user-facing.
import { runKeeper } from "../../worker/src/keeper/multiplexed-main.ts";

export function keeper(args: string[]): void {
  const sock = args[0];
  if (!sock) {
    console.error("usage: roost keeper <socket-path>");
    process.exit(2);
  }
  runKeeper(sock);
}

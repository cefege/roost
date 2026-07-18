// `roost dev` — boot coord (:4102) + worker (:2224) + web (:5174) in
// parallel. Each runs in its own subprocess; SIGINT propagates.

import { spawn, type Subprocess } from "bun";

export async function dev(_args: string[]): Promise<void> {
  const procs: Subprocess[] = [];
  const stop = () => {
    for (const p of procs) p.kill();
    process.exit(0);
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);

  procs.push(spawn({
    cmd: ["bun", "run", "--cwd", "apps/coord", "dev"],
    stdio: ["inherit", "inherit", "inherit"],
    env: { ...process.env, ROOST_COORDINATOR_BIND: "0.0.0.0:4102" },
  }));
  procs.push(spawn({
    cmd: ["bun", "run", "--cwd", "apps/worker", "dev"],
    stdio: ["inherit", "inherit", "inherit"],
  }));
  procs.push(spawn({
    cmd: ["bun", "x", "vite", "--port", "5174"],
    cwd: "apps/web",
    stdio: ["inherit", "inherit", "inherit"],
  }));

  // Wait until any process exits, then stop the rest.
  await Promise.race(procs.map((p) => p.exited));
  stop();
}

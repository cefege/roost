import { open, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { BuiltinSeedContext } from "./builtin-seed";
import type { ServiceTrace } from "./fixture-service-http";
import type { ProcessGeneration } from "./profile-process";

export interface OwnedOAuthFollowRule {
  origin: string;
  targetOrigin: string;
  path: "/approve" | "/authorize";
}

export function startOwnedOAuthFollower(context: BuiltinSeedContext, rules: readonly OwnedOAuthFollowRule[], trace: ServiceTrace): { dispose(): Promise<void> } {
  for (const rule of rules) {
    const target = new URL(rule.targetOrigin);
    if (target.protocol !== "http:" || target.hostname !== "127.0.0.1" || target.origin !== rule.targetOrigin || target.username || target.password) throw new Error("OAuth fixture follow target must be an owned loopback origin");
  }
  const hostTrace = join(context.home, ".parity", "host.jsonl");
  const networkTrace = join(context.home, ".parity", "network.jsonl");
  let stopped = false;
  let failure: unknown;
  const cancellation = new AbortController();
  const pending = (async () => {
    const file = await open(hostTrace, "r");
    let offset = 0;
    let partial = "";
    const bytes = Buffer.alloc(16384);
    try {
      while (!stopped) {
        const read = await file.read(bytes, 0, bytes.length, offset);
        if (read.bytesRead === 0) { await Bun.sleep(20); continue; }
        offset += read.bytesRead;
        partial += bytes.toString("utf8", 0, read.bytesRead);
        const lines = partial.split("\n");
        partial = lines.pop() ?? "";
        for (const line of lines) {
          if (!line) continue;
          const event: unknown = JSON.parse(line);
          if (typeof event !== "object" || event === null || !("channel" in event) || event.channel !== "builtin.host-open-url" || !("value" in event) || typeof event.value !== "string") continue;
          if (!URL.canParse(event.value)) continue;
          const url = new URL(event.value);
          const rule = rules.find(candidate => candidate.origin === url.origin && candidate.path === url.pathname);
          if (!rule || url.username || url.password || url.hash) continue;
          const target = new URL(`${url.pathname}${url.search}`, rule.targetOrigin);
          trace("oauth-follow", { direction: "request", method: "GET", url: url.href, target: target.href, source: "actual-host-open" });
          const response = await fetch(target, { redirect: "manual", credentials: "omit", signal: cancellation.signal });
          if (response.status >= 300 && response.status < 400) {
            const location = response.headers.get("location");
            await response.body?.cancel();
            if (!location || url.pathname !== "/authorize") throw new Error("Owned OAuth approval returned an unexpected redirect");
            const callback = new URL(location, url);
            if (callback.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(callback.hostname) || callback.username || callback.password) throw new Error("OAuth callback redirect escaped owned loopback");
            const registrations = (await readFile(networkTrace, "utf8")).split("\n").filter(Boolean).map(line => JSON.parse(line) as unknown);
            const generation: ProcessGeneration = await Bun.file(join(context.home, ".parity", "process-generation.json")).json();
            if (!Number.isSafeInteger(generation.pid) || generation.pid < 2 || !Number.isSafeInteger(generation.generation) || generation.generation < 1 || resolve(generation.workspace) !== resolve(context.workspace) || resolve(generation.sourceRoot) !== resolve(context.sourceRoot)) throw new Error("OAuth callback generation is not owned by this profile");
            const registered = registrations.some((event, index) => {
              if (typeof event !== "object" || event === null || !("type" in event) || event.type !== "owned-listener-registered" || !("origin" in event) || event.origin !== callback.origin || !("pid" in event) || event.pid !== generation.pid || !("generation" in event) || event.generation !== generation.generation || !("source" in event) || event.source !== "packages/ai/src/registry/oauth/callback-server.ts" || !("listenerId" in event) || typeof event.listenerId !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(event.listenerId)) return false;
              return !registrations.slice(index + 1).some(closed => typeof closed === "object" && closed !== null && "type" in closed && closed.type === "owned-listener-closed" && "listenerId" in closed && closed.listenerId === event.listenerId && "pid" in closed && closed.pid === generation.pid && "generation" in closed && closed.generation === generation.generation);
            });
            if (!registered) throw new Error(`OAuth callback listener has no live current-generation ownership registration: ${callback.origin}`);
            process.kill(generation.pid, 0);
            const completed = await fetch(callback, { redirect: "manual", credentials: "omit", signal: cancellation.signal });
            await completed.body?.cancel();
            trace("oauth-follow", { direction: "callback-response", status: completed.status, origin: callback.origin, path: callback.pathname });
            if (!completed.ok) throw new Error(`Actual OAuth callback refused authorization: HTTP ${completed.status}`);
          } else {
            await response.body?.cancel();
            if (!response.ok) throw new Error(`Owned OAuth authorization request failed: HTTP ${response.status}`);
          }
          trace("oauth-follow", { direction: "completed", origin: url.origin, path: url.pathname });
        }
      }
    } finally { await file.close(); }
  })().catch(error => {
    if (stopped && cancellation.signal.aborted) return;
    failure = error;
    stopped = true;
    trace("oauth-follow", { direction: "failure", message: error instanceof Error ? error.message : String(error) });
  });
  return {
    async dispose() {
      stopped = true;
      cancellation.abort();
      await pending;
      if (failure !== undefined) throw failure;
    },
  };
}

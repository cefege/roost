// The public-origin check in `roost status`. A tunnel or reverse proxy in front
// of an unbound ROOST_PUBLIC_BIND is the exact shape of a browser-visible 502
// that no tailnet probe can see, so this pins that the check reports it.
import { expect, test } from "bun:test";
import {
  publicOriginStatusLine,
  resolvePublicOriginStatus,
} from "../src/status-public-origin.ts";

function deps(
  publicBind: string | null,
  processes: readonly string[],
  listening: boolean,
) {
  return {
    publicBind,
    runningProcessNames: async () => processes,
    isListening: async () => listening,
  };
}

test("a front with no listener on the public bind is reported, not silently healthy", async () => {
  const status = await resolvePublicOriginStatus(
    deps("127.0.0.1:4104", ["bun", "cloudflared", "caddy"], false),
  );
  expect(status).toEqual({
    state: "origin-down",
    bind: "127.0.0.1:4104",
    fronts: ["caddy", "cloudflared"],
  });
  const line = publicOriginStatusLine(status)!;
  expect(line).toContain("✗ public origin 127.0.0.1:4104");
  expect(line).toContain("502");
});

test("a bound public origin behind a front is healthy", async () => {
  const status = await resolvePublicOriginStatus(
    deps("127.0.0.1:4104", ["cloudflared"], true),
  );
  expect(status.state).toBe("healthy");
  expect(publicOriginStatusLine(status)).toContain("✓ public origin");
});

test("no public bind configured reports nothing", async () => {
  const status = await resolvePublicOriginStatus(deps(null, ["cloudflared"], false));
  expect(status).toEqual({ state: "unconfigured" });
  expect(publicOriginStatusLine(status)).toBeNull();
});

test("an unbound public origin with no front is not an error", async () => {
  // A tailnet-only install can legitimately leave the bind unserved.
  const status = await resolvePublicOriginStatus(deps("127.0.0.1:4104", ["bun"], false));
  expect(status.state).toBe("unfronted");
  expect(publicOriginStatusLine(status)).toBeNull();
});

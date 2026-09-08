// The coordinator's caller-address model: one CallerOrigin per request, derived
// from the listener's boot-selected trust profile. Forwarded headers are trusted
// ONLY under "trusted-proxy"; sniffing X-Forwarded-For anywhere else would let a
// client forge its address past rate limits and on-host gates. Callers are
// coord-factory and the Bun listener, which choose the profile from config.
import { Code, ConnectError } from "@connectrpc/connect";

/** How a listener learns the real client address. Chosen per-listener at boot
 * from config — NEVER sniffed from request headers. */
export type ListenerTrust = "direct" | "trusted-proxy";

export interface CallerOrigin {
  /** Boot-selected trust profile for the listener that accepted the request. */
  listener: ListenerTrust;
  /** Real client address for rate limiting and audit. */
  clientIp: string;
  /** True ONLY for a request that originated on the coordinator host and
   * traversed no proxy. Gates the most sensitive endpoints. */
  onHost: boolean;
}

const LOOPBACK: Record<string, true | undefined> = {
  "127.0.0.1": true,
  "::1": true,
  "::ffff:127.0.0.1": true,
};

export function resolveCallerOrigin(
  trust: ListenerTrust,
  socketPeer: string | undefined,
  headers: Headers,
): CallerOrigin {
  if (trust === "direct") {
    return {
      listener: trust,
      clientIp: socketPeer ?? "unknown",
      onHost: socketPeer !== undefined && LOOPBACK[socketPeer] === true,
    };
  }

  // The operator's front door overwrites X-Forwarded-For with the address it
  // authenticated. Its presence therefore proves the request traversed a proxy,
  // which is what disqualifies it from on-host authority.
  const xff = headers.get("x-forwarded-for");
  const forwarded = xff?.split(",")[0]?.trim();
  return {
    listener: trust,
    clientIp: forwarded || socketPeer || "unknown",
    onHost: xff === null && socketPeer !== undefined && LOOPBACK[socketPeer] === true,
  };
}

export function assertOnHost(origin: CallerOrigin): void {
  if (!origin.onHost) {
    throw new ConnectError("on-host only", Code.PermissionDenied);
  }
}

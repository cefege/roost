// The coordinator's caller-address model: one CallerOrigin per request, derived
// from the listener's boot-selected trust profile. Forwarded headers are
// trusted ONLY under "tailscale-serve"; sniffing XFF anywhere else would let
// a client forge its address past rate limits and on-host gates. Application
// enrollment authority never derives from a tailnet address.
import { Code, ConnectError } from "@connectrpc/connect";

/** How a listener learns the real client address. Chosen per-listener at boot
 * from config — NEVER sniffed from request headers. */
export type ListenerTrust = "direct" | "tailscale-serve" | "public-edge";

export interface CallerOrigin {
  /** Boot-selected trust profile for the listener that accepted the request. */
  listener: ListenerTrust;
  /** Real client address for rate limiting and audit. Tailnet classification
   * is applied only while normalizing public-edge proxy addresses. */
  clientIp: string;
  /** True ONLY for a request that originated on the coordinator host and
   * traversed no proxy. Gates the most sensitive endpoints. */
  onHost: boolean;
}

const LOOPBACK = new Set(["127.0.0.1", "::1", "::ffff:127.0.0.1"]);

export function resolveCallerOrigin(
  trust: ListenerTrust,
  socketPeer: string | undefined,
  headers: Headers,
): CallerOrigin {
  if (trust === "direct") {
    return {
      listener: trust,
      clientIp: socketPeer ?? "unknown",
      onHost: socketPeer !== undefined && LOOPBACK.has(socketPeer),
    };
  }

  if (trust === "tailscale-serve") {
    const xff = headers.get("x-forwarded-for");
    const forwarded = xff?.split(",")[0]?.trim();
    return {
      listener: trust,
      clientIp: forwarded || socketPeer || "unknown",
      onHost: xff === null && socketPeer !== undefined && LOOPBACK.has(socketPeer),
    };
  }

  const connectingIp = headers.get("cf-connecting-ip")?.trim() ?? "";
  return {
    listener: trust,
    clientIp: !connectingIp || LOOPBACK.has(connectingIp) || isTailnetAddr(connectingIp)
      ? "public"
      : connectingIp,
    onHost: false,
  };
}

// Tailscale assigns every node a 100.64.0.0/10 (CGNAT) IPv4 and a
// fd7a:115c:a1e0::/48 ULA IPv6.
export function isTailnetAddr(remoteAddress: string): boolean {
  const v4 = remoteAddress.startsWith("::ffff:") ? remoteAddress.slice(7) : remoteAddress;
  const octets = v4.split(".");
  if (octets.length === 4) {
    const first = Number(octets[0]);
    const second = Number(octets[1]);
    return first === 100 && second >= 64 && second <= 127;
  }
  return remoteAddress.toLowerCase().startsWith("fd7a:115c:a1e0");
}

export function assertOnHost(origin: CallerOrigin): void {
  if (!origin.onHost) {
    throw new ConnectError("on-host only", Code.PermissionDenied);
  }
}

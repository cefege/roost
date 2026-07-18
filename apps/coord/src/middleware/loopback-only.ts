// Source-address guards for admin endpoints. Checks socket remote address
// — no XFF honored (per REWRITE.md R1.1 security baseline). Throws
// ConnectError(PermissionDenied) so Connect maps to HTTP 403; the SPA's
// `_attemptSelfRegister` distinguishes expected denial (403, falls through
// to Onboarding) from real coord failure (500). Prior shape threw TRPCError
// which Connect couldn't recognize, surfacing as generic 500 and breaking
// the SPA bootstrap path on every tailnet load.
//
// Two guards, two threat models:
//   - assertLoopback           — strict. Caller must be ON the coord host.
//     Used by miscDbExportUrl (full DB dump — most sensitive).
//   - assertLoopbackOrTailnet  — the tailnet IS the trust boundary. Used by
//     authAuthorizeBrowser (zero-credential browser self-register). Any
//     WireGuard-authenticated peer on the author's single-account tailnet may
//     mint a browser key over the FQDN, so a fresh browser onboards without
//     loopback/SSH access to the coord host (the "all FQDN for simplicity"
//     decision). A non-tailnet LAN peer (192.168.x hitting the 0.0.0.0
//     bind) is still rejected — only loopback or 100.64.0.0/10 passes.
//     Source is the real socket peer from server.requestIP() (un-spoofable
//     over a real TCP handshake; XFF ignored), so the boundary holds.
//
// The remote address must be captured at the runtime layer (Bun.serve's
// server.requestIP() / Node http req.socket.remoteAddress) and threaded
// through via the x-roost-remote-addr header — by the time we reach a
// Connect handler we only have a synthesized fetch Request which has no
// socket. The auth interceptor reads the header and stashes it on
// `remoteAddressKey`.

import { Code, ConnectError } from "@connectrpc/connect";

const LOOPBACK = new Set(["127.0.0.1", "::1", "::ffff:127.0.0.1"]);

export function assertLoopback(remoteAddress: string | undefined): void {
  if (!remoteAddress || !LOOPBACK.has(remoteAddress)) {
    throw new ConnectError("loopback only", Code.PermissionDenied);
  }
}

// Tailscale assigns every node a 100.64.0.0/10 (CGNAT) IPv4 and a
// fd7a:115c:a1e0::/48 ULA IPv6. Returns true for either; callers OR this
// with the loopback check.
function isTailnetAddr(remoteAddress: string): boolean {
  const v4 = remoteAddress.startsWith("::ffff:") ? remoteAddress.slice(7) : remoteAddress;
  const octets = v4.split(".");
  if (octets.length === 4) {
    const first = Number(octets[0]);
    const second = Number(octets[1]);
    return first === 100 && second >= 64 && second <= 127; // 100.64.0.0/10
  }
  return remoteAddress.toLowerCase().startsWith("fd7a:115c:a1e0"); // tailscale ULA
}

export function assertLoopbackOrTailnet(remoteAddress: string | undefined): void {
  if (!remoteAddress || (!LOOPBACK.has(remoteAddress) && !isTailnetAddr(remoteAddress))) {
    throw new ConnectError("loopback or tailnet only", Code.PermissionDenied);
  }
}

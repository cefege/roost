// Health of the coordinator's fronted public origin: a tunnel or reverse proxy
// in front of ROOST_PUBLIC_BIND is useless if nothing is listening on that
// bind. Called by status-report.ts and rendered by status-output.ts; the probes
// are injected so the decision is testable without a host.

/** Processes that front the public bind on a deployed host. A running one plus
 * an unbound origin is the shape a browser sees as a gateway error, which no
 * check on the tailnet URL can observe: the private bind stays healthy. */
const PUBLIC_ORIGIN_FRONTS: Record<string, true> = {
  cloudflared: true,
  caddy: true,
  nginx: true,
  haproxy: true,
};

export type PublicOriginStatus =
  | { state: "unconfigured" }
  | { state: "unfronted"; bind: string }
  | { state: "healthy"; bind: string; fronts: readonly string[] }
  | { state: "origin-down"; bind: string; fronts: readonly string[] };

export interface PublicOriginDeps {
  /** ROOST_PUBLIC_BIND from the installed coordinator service definition. */
  publicBind: string | null;
  /** Names of processes running on this host. */
  runningProcessNames: () => Promise<readonly string[]>;
  /** Whether anything accepts connections on `host:port`. */
  isListening: (bind: string) => Promise<boolean>;
}

export async function resolvePublicOriginStatus(
  deps: PublicOriginDeps,
): Promise<PublicOriginStatus> {
  const bind = deps.publicBind?.trim();
  if (!bind) return { state: "unconfigured" };

  const names = await deps.runningProcessNames();
  const fronts = [...new Set(names.filter((name) => PUBLIC_ORIGIN_FRONTS[name] === true))].sort();
  if (fronts.length === 0) return { state: "unfronted", bind };

  // A service manager can report a front's unit inactive while the process
  // itself runs, so the running process — not the unit — is the evidence.
  return await deps.isListening(bind)
    ? { state: "healthy", bind, fronts }
    : { state: "origin-down", bind, fronts };
}

export function publicOriginStatusLine(status: PublicOriginStatus): string | null {
  switch (status.state) {
    case "unconfigured":
    case "unfronted":
      return null;
    case "healthy":
      return `  ✓ public origin ${status.bind} (fronted by ${status.fronts.join(", ")})`;
    case "origin-down":
      return `  ✗ public origin ${status.bind} has no listener while ${
        status.fronts.join(", ")
      } fronts it\n      → every request through that front answers 502; `
        + "restore ROOST_PUBLIC_BIND on the coordinator service";
  }
}

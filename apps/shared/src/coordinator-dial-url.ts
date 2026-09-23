// Resolves coordinator origins for worker dialing and remote enrollment.
// The deploy composer and remote enrollment paths share this policy so a declared
// front door has one precedence and validation rule.
// Empty entries count as undeclared; a nonblank remote enrollment declaration is
// authoritative and invalid values refuse instead of falling back.

/** Precedence order: an explicit worker target beats the coordinator identity
 * origin, which beats the browser front door. */
export const COORDINATOR_DIAL_URL_ENV_NAMES = [
  "ROOST_COORDINATOR_URL",
  "ROOST_COORDINATOR_PUBLIC_URL",
  "ROOST_WEB_PUBLIC_URL",
] as const;

export const COORDINATOR_DIAL_URL_REQUIRED_MESSAGE =
  `no coordinator URL is configured: set ${COORDINATOR_DIAL_URL_ENV_NAMES.join(", ")}`;

export function resolveCoordinatorDialUrl(
  env: Record<string, string | undefined>,
): string | null {
  for (const name of COORDINATOR_DIAL_URL_ENV_NAMES) {
    // Installed service definitions carry declared-but-empty entries
    // (Environment="ROOST_COORDINATOR_PUBLIC_URL="); those are not a URL.
    const declared = env[name]?.trim();
    if (declared) return declared;
  }
  return null;
}

export function workerCoordinatorUrl(
  declaredUrl: string | null | undefined,
  activeCoordinatorOrigin: string,
): string | null {
  const declared = declaredUrl?.trim();
  if (declared) return validWorkerOrigin(declared);
  return validWorkerOrigin(activeCoordinatorOrigin);
}

function validWorkerOrigin(candidate: string): string | null {
  try {
    const url = new URL(candidate);
    if (
      url.protocol !== "https:"
      || url.username
      || url.password
      || url.pathname !== "/"
      || url.search
      || url.hash
      // A worker on another machine cannot dial a loopback address.
      || /^(?:localhost|.*\.localhost|127(?:\.\d{1,3}){3}|\[::1\])\.?$/.test(url.hostname)
    ) return null;
    return url.origin;
  } catch {
    return null;
  }
}

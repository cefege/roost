// Resolves the coordinator origin a worker should dial from an environment map.
// The coordinator's deploy job composer and the roost-cli enrollment commands both
// read it, so an operator's declared front door is interpreted identically on both
// sides. Roost never derives this origin: an empty entry counts as undeclared, and
// when nothing declares one the caller refuses with COORDINATOR_DIAL_URL_REQUIRED_MESSAGE.

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

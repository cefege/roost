// Resolves the HTTPS origin a new worker should dial, for the Add machine command.
// MachineDeployDialog calls it with the coordinator's declared public URL and the
// origin of the coordinator that client is actually talking to; nothing else.
//
// A declared coordinator URL is authoritative: when it is present but unusable the
// answer is a refusal, never the fallback, because masking a malformed
// ROOST_COORDINATOR_PUBLIC_URL would hand out an enrollment command for a
// different door than the operator declared. The fallback exists for the standard
// install, which declares nothing but ROOST_WEB_PUBLIC_URL — the origin the active
// coordinator is already served on.

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

export function workerCoordinatorUrl(
  declaredUrl: string | null | undefined,
  activeCoordinatorOrigin: string,
): string | null {
  const declared = declaredUrl?.trim();
  if (declared) return validWorkerOrigin(declared);
  return validWorkerOrigin(activeCoordinatorOrigin);
}

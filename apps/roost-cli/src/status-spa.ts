// Owns what `roost status` can say about the browser build: the dist the
// installed service stamped, whether that path still holds one, and whether the
// coordinator's own listener actually answers a page request. Reads the unit
// through status-service-env.ts and the disk check through @roost/host/spa,
// so nothing here re-derives an existence rule the server already owns.

import { resolveDiskSpaRoot } from "@roost/host/spa";
import { serviceEnvironmentValue } from "./status-service-env.ts";
import type { SpaStatus } from "./status-types.ts";

/** What the installed coordinator does with a page request, plus the dist its
 *  service definition stamped. A deploy points that path INTO a release
 *  directory a later settlement deletes, so the path and the served state are
 *  reported separately rather than one inferred from the other. */
export async function resolveSpaStatus(
  serviceDefinition: string | null,
  /** The coordinator's own loopback listener, never a front door. */
  coordUrl: string | null,
  platform: NodeJS.Platform = process.platform,
  fetchImpl: typeof fetch = fetch,
): Promise<SpaStatus> {
  const declared = serviceDefinition
    ? serviceEnvironmentValue(serviceDefinition, "ROOST_WEB_DIST_PATH", platform)?.trim()
    : null;
  const webDistPath = declared ? declared : null;
  return {
    serves: await _probeSpaRoot(coordUrl, fetchImpl),
    webDistPath,
    webDistPresent: resolveDiskSpaRoot(webDistPath ?? undefined) !== null,
  };
}

/** HEAD the coordinator's own root. A page request is the only authority on
 *  whether a build is being served: `createSpaResponder` picks disk-vs-embed
 *  once at boot, so a dist created after the coordinator started is not served
 *  however current the configuration looks, and a compiled install answers
 *  from an embedded manifest this CLI cannot read. Loopback only — coord's SPA
 *  arm 404s a request it cannot see as on-host whenever Cloudflare Access is
 *  configured. */
export async function _probeSpaRoot(
  coordUrl: string | null,
  fetchImpl: typeof fetch = fetch,
): Promise<boolean | null> {
  if (!coordUrl) return null;
  try {
    const response = await fetchImpl(`${coordUrl}/`, {
      method: "HEAD",
      signal: AbortSignal.timeout(5000),
    });
    return response.status === 200;
  } catch {
    return null;
  }
}

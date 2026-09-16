// One reader for an installed coordinator service definition's environment.
// `roost status` resolvers (endpoint, database path, SPA) all ask this instead
// of re-deriving the per-platform shape: systemd/launchd definitions parse
// through deploy-plist-env.ts, Windows keeps its definitions as JSON.

import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";

export function serviceEnvironmentValue(
  serviceDefinition: string,
  name: string,
  platform: NodeJS.Platform,
): string | null {
  switch (platform) {
    case "darwin":
    case "linux":
      return parsePosixServiceEnvironment(serviceDefinition, platform)[name] ?? null;
    case "win32": {
      try {
        const stored = JSON.parse(serviceDefinition) as {
          services?: { coordinator?: { environment?: Record<string, unknown> } };
        };
        const value = stored.services?.coordinator?.environment?.[name];
        return typeof value === "string" ? value : null;
      } catch {
        return null;
      }
    }
    default:
      throw new Error(`unsupported coordinator service platform: ${platform}`);
  }
}

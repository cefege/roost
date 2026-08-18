export const SUPPORTED_HOST_PLATFORMS = ["darwin", "linux", "win32"] as const;

export type SupportedHostPlatform = (typeof SUPPORTED_HOST_PLATFORMS)[number];

export function isSupportedHostPlatform(value: unknown): value is SupportedHostPlatform {
  return value === "darwin" || value === "linux" || value === "win32";
}

/** Resolve a runtime platform without allowing an unknown host to fall through. */
export function supportedHostPlatform(
  value: string = typeof process !== "undefined" ? process.platform : "",
): SupportedHostPlatform {
  if (isSupportedHostPlatform(value)) return value;
  throw new Error(`unsupported host platform: ${value || "unknown"}`);
}

export function assertNeverPlatform(value: never): never {
  throw new Error(`unhandled host platform: ${String(value)}`);
}

export function platformName(platform: SupportedHostPlatform): "macOS" | "Linux" | "Windows" {
  switch (platform) {
    case "darwin": return "macOS";
    case "linux": return "Linux";
    case "win32": return "Windows";
    default: return assertNeverPlatform(platform);
  }
}

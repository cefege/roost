export function workerCoordinatorUrl(
  configuredUrl: string | null | undefined,
  browserOrigin: string,
): string | null {
  if (!configuredUrl) return null;
  try {
    const url = new URL(configuredUrl);
    if (
      url.protocol !== "https:"
      || url.username
      || url.password
      || url.pathname !== "/"
      || url.search
      || url.hash
      || /^(?:localhost|.*\.localhost|127(?:\.\d{1,3}){3}|\[::1\])\.?$/.test(url.hostname)
      || url.origin === new URL(browserOrigin).origin
    ) return null;
    return url.origin;
  } catch {
    return null;
  }
}

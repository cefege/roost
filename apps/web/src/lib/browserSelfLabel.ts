// Derive a human-readable label for THIS browser, sent to coord at pair
// time (Onboarding redeemToken + startPairFlow). Stored in
// authorized_keys.label and surfaced on viewer chips so other browsers
// see "Chrome — macOS" instead of an 8-char fp prefix.
// Browsers can't read OS hostname (no API), so we approximate from
// userAgentData (modern Chromium) with a UA fallback.

interface UAData { brands?: { brand: string; version: string }[]; platform?: string }

export function browserSelfLabel(): string {
  const nav = navigator as Navigator & { userAgentData?: UAData };
  const platform = nav.userAgentData?.platform || _platformFromUA(navigator.userAgent);
  const browser = _browserFromUAData(nav.userAgentData?.brands) || _browserFromUA(navigator.userAgent);
  if (browser && platform) return `${browser} — ${platform}`;
  if (browser) return browser;
  if (platform) return platform;
  return "browser";
}

function _browserFromUAData(brands?: { brand: string; version: string }[]): string | null {
  if (!brands || brands.length === 0) return null;
  const real = brands.find((b) => !/Not.A.Brand|Chromium/i.test(b.brand));
  return real?.brand ?? brands[0]?.brand ?? null;
}

function _browserFromUA(ua: string): string | null {
  if (/Firefox\//.test(ua)) return "Firefox";
  if (/Edg\//.test(ua)) return "Edge";
  if (/Chrome\//.test(ua)) return "Chrome";
  if (/Safari\//.test(ua)) return "Safari";
  return null;
}

function _platformFromUA(ua: string): string | null {
  if (/Mac OS X|Macintosh/.test(ua)) return "macOS";
  if (/Windows/.test(ua)) return "Windows";
  if (/Linux/.test(ua)) return "Linux";
  if (/iPhone|iPad/.test(ua)) return "iOS";
  if (/Android/.test(ua)) return "Android";
  return null;
}

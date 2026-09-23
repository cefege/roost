// Response security headers shared by every roost HTTP listener: the
// coordinator's public door (apps/coord/src/middleware/security.ts) and the
// worker's loopback UI server (apps/worker/src/local-ui-server.ts). One CSP
// builder keeps the two doors from drifting into different connect-src rules
// for the same SPA bundle.

const CSP_TAIL = "frame-ancestors 'none'";

export function buildCsp(
  relaxed: boolean,
  connectOrigins: string[],
): string {
  const connections = new Set(["'self'", ...connectOrigins]);
  if (relaxed) {
    connections.add("http:");
    connections.add("ws:");
  }
  const scriptSources = ["'self'", "'wasm-unsafe-eval'", "blob:"];
  const directives = [
    "default-src 'self'",
    `script-src ${scriptSources.join(" ")}`,
    "worker-src 'self' blob:",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self' data:",
    "base-uri 'self'",
    "form-action 'none'",
    "object-src 'none'",
  ];
  return `${directives.join("; ")}; connect-src ${[...connections].join(" ")}; ${CSP_TAIL}`;
}

export function applySecurityHeaders(
  headers: Headers,
  relaxed: boolean,
  hsts: boolean,
  connectOrigins: string[],
): void {
  headers.set("content-security-policy", buildCsp(relaxed, connectOrigins));
  headers.set("x-frame-options", "DENY");
  headers.set("x-content-type-options", "nosniff");
  headers.set("referrer-policy", "no-referrer");
  headers.set("permissions-policy", "camera=(), geolocation=(), microphone=(self)");
  if (hsts) headers.set("strict-transport-security", "max-age=31536000");
}

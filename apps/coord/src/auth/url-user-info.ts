// One definition of "this URL carries embedded credentials", used by every
// coordinator surface that accepts an operator- or browser-supplied URL:
// push subscription endpoints (connect/handlers-push.ts, push-dispatch.ts) and
// signed Windows update manifests (windows-update-deploy-record.ts). Several
// hand-written copies of one rejection rule is exactly how one surface
// silently keeps accepting them.

/**
 * True when the URL's authority carries a userinfo component. WHATWG
 * serialization places userinfo only before an `@` in the authority and
 * percent-encodes any `@` inside it, and a host can never contain a literal
 * `@`, so a literal `@` ahead of the path is proof and the only proof.
 */
export function hasUrlUserInfo(url: URL): boolean {
  const afterScheme = url.href.slice(url.protocol.length + 2);
  const authorityEnd = afterScheme.search(/[/?#]/);
  const authority = authorityEnd === -1
    ? afterScheme
    : afterScheme.slice(0, authorityEnd);
  return authority.includes("@");
}

// Shared, pure Connect-handler helpers — no router/module state. Importable
// by router.ts AND the split connect/handlers-*.ts domain files without a
// runtime import cycle (router.ts ↔ handlers-* would otherwise both import
// each other's values). Add a helper here when >1 handler file needs it.

import { Code, ConnectError } from "@connectrpc/connect";
import type { Caller } from "./auth-interceptor.ts";
import type { ClientControlFrame } from "@roost/shared/wire";

export type WorkerHubSocket = { send(data: string | Uint8Array): void };

// `${prefix}<bytes*2 hex>` random id. Used by auth/pair/webhook token mint.
export function randomToken(prefix: string, bytes: number): string {
  const buf = new Uint8Array(bytes);
  crypto.getRandomValues(buf);
  return `${prefix}${Buffer.from(buf).toString("hex")}`;
}

export async function sha256hex(s: string): Promise<string> {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return Buffer.from(new Uint8Array(d)).toString("hex");
}

// Wire-boundary validation for proto3 required-string / required-array
// fields. Proto3 has no length facet so an unset field arrives as "" or [].
// Used by webhookTokensMint, permissionsCreate, mcpCreate.
export function requireNonEmpty(value: string | unknown[], field: string): void {
  if (value.length === 0) {
    throw new ConnectError(`${field} is required`, Code.InvalidArgument);
  }
}

// Single source of truth for the browser-command JSON envelope. Used by
// every Connect handler that forwards a request through the worker hub.
// Throws ConnectError on send failure so handlers can `await pending.promise`
// immediately after.
export function sendBrowserCmd(
  sock: WorkerHubSocket,
  caller: Caller,
  requestId: string,
  frame: ClientControlFrame,
  overrideViewerId?: string,
): void {
  const downstream = {
    kind: "browser-command" as const,
    browser_id: caller.fingerprint,
    // Resize handler passes a `${fp}:${tab_id}` composite so worker's
    // viewportClaims map disambiguates tabs from the same browser.
    // Other call sites omit the override and fall back to the raw fp.
    viewer_id: overrideViewerId ?? caller.fingerprint,
    request_id: requestId,
    frame,
  };
  try { sock.send(JSON.stringify(downstream)); }
  catch (e) {
    throw new ConnectError(`send failed: ${String(e)}`, Code.Unavailable);
  }
}

import { appendFileSync } from "node:fs";
import { deniedDestinationPrefix, isModelDiscoveryStack } from "./network-classification";

const origins = new Set((process.env.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").filter(Boolean));
const permanentOrigins = new Set(origins);
const listeners = new Map<string, Map<string, OwnedLoopbackListener>>();
const trace = process.env.OMP_PARITY_NETWORK_TRACE;
const transport = globalThis.fetch;
if (!trace || process.env.OMP_PARITY_ALLOWED_ORIGINS === undefined) throw new Error("Parity network guard requires an owned trace and explicit allowed origins");
appendFileSync(trace, `${JSON.stringify({ type: "guard-installed", allowedOrigins: [...origins] })}\n`);

export interface OwnedLoopbackListener {
  pid: number;
  generation: number;
  listenerId: string;
  source: string;
  sourcePath: string;
  hostname: string;
  port: number;
  listenerIds?: string[];
}

export function allowOwnedLoopbackOrigin(origin: string, listener?: OwnedLoopbackListener): void {
  const url = new URL(origin);
  const hostAllowed = url.hostname === "127.0.0.1" || (listener !== undefined && ["localhost", "[::1]"].includes(url.hostname));
  if (url.protocol !== "http:" || !hostAllowed || !url.port || Number(url.port) > 65535 || url.username || url.password || url.pathname !== "/" || url.search || url.hash || url.origin !== origin) throw new Error(`Invalid owned loopback listener origin: ${origin}`);
  if (listener && (listener.pid !== process.pid || !Number.isSafeInteger(listener.generation) || listener.generation < 1 || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(listener.listenerId) || listener.port !== Number(url.port))) throw new Error("Invalid owned listener provenance");
  appendFileSync(trace!, `${JSON.stringify({ type: "owned-listener-registered", origin, ...listener })}\n`);
  if (listener) {
    const owners = listeners.get(origin) ?? new Map<string, OwnedLoopbackListener>();
    owners.set(listener.listenerId, listener);
    listeners.set(origin, owners);
  } else permanentOrigins.add(origin);
  origins.add(origin);
}

export function revokeOwnedLoopbackOrigin(origin: string, listenerId: string): void {
  const owners = listeners.get(origin);
  const listener = owners?.get(listenerId);
  if (!listener || !owners) throw new Error("Cannot retire an unobserved owned listener");
  owners.delete(listenerId);
  if (!owners.size) {
    listeners.delete(origin);
    if (!permanentOrigins.has(origin)) origins.delete(origin);
  }
  appendFileSync(trace!, `${JSON.stringify({ type: "owned-listener-closed", origin, ...listener })}\n`);
}

function checkedUrl(input: RequestInfo | URL): URL {
  const url = new URL(input instanceof Request ? input.url : input.toString());
  if (origins.has(url.origin)) return url;
  const failure = `${deniedDestinationPrefix} ${url.origin}${url.pathname}`;
  const stack = new Error(failure).stack ?? "";
  if (trace) appendFileSync(trace, `${JSON.stringify({ error: failure, ...(isModelDiscoveryStack(stack) ? { kind: "model-discovery" } : {}), stack })}\n`);
  throw new Error(failure);
}

async function guardedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  checkedUrl(input);
  return transport(input, init);
}

globalThis.fetch = Object.assign(guardedFetch, {
  preconnect: (url: string | URL, options?: { dns?: boolean; tcp?: boolean; http?: boolean; https?: boolean }) => {
    checkedUrl(url);
    transport.preconnect(url, options);
  },
});

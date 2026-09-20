// Browser-context faults owned by the terminal smoke harness.
// They are installed before any document script and disappear with that context.
// The loopback fault intercepts only the local bootstrap request, never ICE/UDP.
// No product environment variable or command enables either fault.

import type { BrowserContext } from "@playwright/test";

declare global {
  interface Window { __releaseTerminalPeerAnswer?: () => void; }
}

const DISABLED_LOCAL_WORKER_ORIGIN = "http://127.0.0.1:1";

/** Prevent only local-door discovery so a coordinator page must stage peer or Sync. */
export async function installDisabledLoopbackProbe(context: BrowserContext): Promise<void> {
  await context.addInitScript((blockedOrigin: string) => {
    localStorage.setItem("roost.localWorkerOrigin", blockedOrigin);
    const fetchWithoutLoopbackProbe = window.fetch.bind(window);
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const requestUrl = typeof input === "string"
        ? new URL(input, location.href)
        : input instanceof URL
          ? input
          : new URL(input.url);
      if (requestUrl.origin === blockedOrigin && requestUrl.pathname === "/api/local-bootstrap") {
        return Promise.reject(new TypeError("terminal smoke loopback probe disabled"));
      }
      return fetchWithoutLoopbackProbe(input, init);
    }) as typeof window.fetch;
  }, DISABLED_LOCAL_WORKER_ORIGIN);
}

/** Remove the peer API before app code loads; fallback must use ordinary Sync. */
export async function installRtcUnavailable(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    Object.defineProperty(window, "RTCPeerConnection", {
      configurable: true,
      value: undefined,
    });
  });
}

/** Hold answer installation after worker signaling so a Sync frame can lag behind it. */
export async function installRtcAnswerHold(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    const original = RTCPeerConnection.prototype.setRemoteDescription;
    const gate = Promise.withResolvers<void>();
    const setRemoteDescription = original as (
      this: RTCPeerConnection,
      description: RTCSessionDescriptionInit,
    ) => Promise<void>;
    window.__releaseTerminalPeerAnswer = () => gate.resolve();
    const heldSetRemoteDescription = async function (
      this: RTCPeerConnection,
      description: RTCSessionDescriptionInit,
    ): Promise<void> {
      await gate.promise;
      return setRemoteDescription.call(this, description);
    };
    RTCPeerConnection.prototype.setRemoteDescription =
      heldSetRemoteDescription as typeof original;
  });
}

// Settings → "Pair a device" pane. Renders a QR for the current HTTPS origin.
// Its one-shot bearer stays in the URL fragment, so Cloudflare, access logs,
// and Referer headers never receive it.
//
// Any non-loopback HTTPS origin is phone-reachable when the phone can route to
// it: either the public Roost domain or a tailnet URL.

import { createSignal, onMount, Show } from "solid-js";
import QRCode from "qrcode";
import { coordClient } from "../../connect.ts";
import { addToast } from "../../store/toastStore.ts";
import { Button } from "./md/primitives.tsx";

function originIsPhoneReachable(): boolean {
  if (typeof location === "undefined") return false;
  if (location.protocol !== "https:") return false;
  return !/^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);
}

export function PairDevicePane() {
  const [qrDataUrl, setQrDataUrl] = createSignal<string | null>(null);
  const [pairUrl, setPairUrl] = createSignal<string>("");
  const [status, setStatus] = createSignal<"idle" | "loading" | "ready" | "error">("idle");
  const [errorMsg, setErrorMsg] = createSignal("");

  const reachable = originIsPhoneReachable();

  async function mintAndRender() {
    setStatus("loading");
    try {
      const { token } = await coordClient.authMintBootstrap({ kind: "browser", label: "phone" });
      const url = `${location.origin}/#pair=${encodeURIComponent(token)}`;
      setPairUrl(url);
      setQrDataUrl(await QRCode.toDataURL(url, { width: 240, margin: 1 }));
      setStatus("ready");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setStatus("error");
      setErrorMsg(msg);
      addToast(`Could not create pairing code: ${msg}`, "err");
    }
  }

  onMount(() => {
    if (reachable) void mintAndRender();
  });

  return (
    <div data-testid="pair-device-pane" style={{ padding: "8px 4px", color: "var(--md-sys-color-on-surface)", "max-width": "520px" }}>
      <Show
        when={reachable}
        fallback={
          <div
            data-testid="pair-device-unreachable"
            style={{
              background: "var(--md-sys-color-surface-container)",
              border: "1px solid var(--md-sys-color-outline)",
              "border-radius": "var(--md-shape-sm)",
              padding: "20px",
              "font-size": "13px",
              "line-height": "1.6",
              color: "var(--md-sys-color-on-surface-variant)",
            }}
          >
            <div style={{ "font-weight": 600, color: "var(--md-sys-color-on-surface)", "margin-bottom": "8px" }}>
              Open Roost at an address your phone can reach
            </div>
            Open the address your phone can reach (your public Roost domain, or
            your tailnet URL) on this device, then return to Settings → Pair a
            device.
          </div>
        }
      >
        <p style={{ "font-size": "13px", color: "var(--md-sys-color-on-surface-variant)", "margin-bottom": "20px", "line-height": "1.5" }}>
          Scan this with your phone's camera. The phone must be able to reach
          this address. It opens Roost and pairs automatically — no typing.
        </p>

        <div style={{ display: "flex", "flex-direction": "column", "align-items": "flex-start", gap: "14px" }}>
          <Show when={status() === "ready" && qrDataUrl()} fallback={
            <div style={{ width: "240px", height: "240px", display: "grid", "place-items": "center", background: "var(--md-sys-color-surface-container)", border: "1px solid var(--md-sys-color-outline)", "border-radius": "var(--md-shape-sm)", color: "var(--md-sys-color-on-surface-variant)", "font-size": "13px" }}>
              <Show when={status() === "error"} fallback={<span data-testid="pair-device-loading">Generating…</span>}>
                <span style={{ color: "var(--md-sys-color-error)" }}>Error: {errorMsg()}</span>
              </Show>
            </div>
          }>
            <img
              data-testid="pair-device-qr"
              src={qrDataUrl()!}
              alt="Pairing QR code"
              width="240"
              height="240"
              style={{ background: "#fff", padding: "8px", "border-radius": "var(--md-shape-sm)" }}
            />
          </Show>

          <Show when={status() === "ready"}>
            <div style={{ "font-size": "11px", "font-family": "ui-monospace, SFMono-Regular, Menlo, monospace", color: "var(--md-sys-color-on-surface-variant)", "word-break": "break-all", "max-width": "260px" }}>
              {pairUrl()}
            </div>
          </Show>

          <Button
            variant="tonal"
            data-testid="pair-device-regenerate"
            onClick={mintAndRender}
            disabled={status() === "loading"}
          >
            {status() === "loading" ? "…" : status() === "ready" ? "New code" : "Generate code"}
          </Button>
        </div>
      </Show>
    </div>
  );
}

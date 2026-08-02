// Settings → Devices: authorized browser identities, phone pairing, and pending
// browser approvals.

import { For, Show, createResource, createSignal } from "solid-js";
import { coordClient } from "../../connect.ts";
import {
  getCurrentWebKeyInfo,
  rotateCurrentWebKey,
} from "../../auth/web-key.ts";
import { browserSelfLabel } from "../../lib/browserSelfLabel.ts";
import { addToast } from "../../lib/toastStore.ts";
import { Card, Button } from "./md/primitives.tsx";
import { PairDevicePane } from "./PairDevicePane.tsx";
import { Onboarding } from "../Onboarding.tsx";

export function DevicesPane() {
  const [devices, { refetch }] = createResource(() => coordClient.devicesList({}));
  const [keyInfo] = createResource(getCurrentWebKeyInfo);
  const [busyFingerprint, setBusyFingerprint] = createSignal<string | null>(null);

  async function revoke(fingerprint: string, label: string): Promise<void> {
    if (!confirm(`Revoke ${label || fingerprint.slice(0, 12)}? This browser will need to pair again.`)) return;
    setBusyFingerprint(fingerprint);
    try {
      await coordClient.devicesRevoke({ fingerprint });
      await refetch();
      addToast("Device revoked", "ok");
    } catch (error) {
      addToast(`Revoke failed: ${error instanceof Error ? error.message : String(error)}`, "err");
    } finally {
      setBusyFingerprint(null);
    }
  }

  async function rotate(): Promise<void> {
    setBusyFingerprint("rotate");
    try {
      await rotateCurrentWebKey(browserSelfLabel());
    } catch (error) {
      addToast(`Key upgrade failed: ${error instanceof Error ? error.message : String(error)}`, "err");
      setBusyFingerprint(null);
    }
  }

  return (
    <div data-testid="settings-devices-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Authorized devices"
        supporting="Browsers authorized to access this coordinator. Revocation is permanent for that key."
      >
        <Show when={devices.loading}>
          <p style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>Loading devices…</p>
        </Show>
        <Show when={devices.error}>
          <p role="alert" style={{ margin: "0", color: "var(--md-sys-color-error)" }}>
            {String(devices.error)}
          </p>
        </Show>
        <div style={{ display: "flex", "flex-direction": "column", gap: "12px" }}>
          <For each={devices()?.devices ?? []}>
            {(device) => (
              <div
                data-testid={`authorized-device-${device.fingerprint}`}
                style={{ display: "flex", "align-items": "center", gap: "12px", "justify-content": "space-between" }}
              >
                <div style={{ "min-width": "0" }}>
                  <div style={{ "font-weight": "600" }}>
                    {device.label || "Unnamed browser"}
                    <Show when={device.isSelf}> <span style={{ color: "var(--md-sys-color-primary)" }}>This device</span></Show>
                  </div>
                  <div style={{ color: "var(--md-sys-color-on-surface-variant)", "font-size": "12px", "font-family": "monospace" }}>
                    {device.fingerprint} · {new Date(Number(device.addedAtMs)).toLocaleString()}
                  </div>
                </div>
                <Show
                  when={!device.isSelf}
                  fallback={
                    <Show when={keyInfo()?.extractable}>
                      <Button variant="tonal" disabled={busyFingerprint() !== null} onClick={() => void rotate()}>
                        Upgrade key security
                      </Button>
                    </Show>
                  }
                >
                  <Button
                    variant="tonal"
                    disabled={busyFingerprint() !== null}
                    onClick={() => void revoke(device.fingerprint, device.label)}
                  >
                    {busyFingerprint() === device.fingerprint ? "Revoking…" : "Revoke"}
                  </Button>
                </Show>
              </div>
            )}
          </For>
        </div>
      </Card>
      <Card
        title="Pair a phone"
        supporting="Scan this with your phone's camera. Roost opens and pairs automatically — no typing."
      >
        <PairDevicePane />
      </Card>
      <Card
        title="Approve a browser"
        supporting="When you open Roost in a new browser it requests access. Approve the pending request here from a browser that's already paired."
      >
        <Onboarding embedded />
      </Card>
    </div>
  );
}

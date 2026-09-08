// Settings → Devices: the browser identities authorized on this coordinator,
// plus the phone-pairing QR and the pending browser-approval queue.
// Reads devicesList/devicesRevoke over coordClient and the local key info from
// auth/web-key.ts; PairDevicePane and Onboarding own the two pairing surfaces.

import { For, Show, createResource, createSignal } from "solid-js";
import { coordClient } from "../../connect.ts";
import {
  getCurrentWebKeyInfo,
  rotateCurrentWebKey,
} from "../../auth/web-key.ts";
import { browserSelfLabel } from "../../lib/browserSelfLabel.ts";
import { addToast } from "../../store/toastStore.ts";
import { Card, Button, EmptyState, List, ListRow } from "./md/primitives.tsx";
import { PairDevicePane } from "./PairDevicePane.tsx";
import { Onboarding } from "../Onboarding.tsx";

function AuthorizedDevicesCard() {
  const [devices, { refetch }] = createResource(() => coordClient.devicesList({}));
  const [keyInfo] = createResource(async () => getCurrentWebKeyInfo());
  const [busyFingerprint, setBusyFingerprint] = createSignal<string | null>(null);

  async function revoke(fingerprint: string, label: string): Promise<void> {
    const displayLabel = label || fingerprint.slice(0, 12);
    if (!confirm(`Revoke ${displayLabel}? This browser will need to pair again.`)) return;

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
    } finally {
      setBusyFingerprint(null);
    }
  }

  return (
    <Card
      title="Authorized devices"
      supporting="Browsers authorized to access this coordinator. Revocation is permanent for that key."
    >
      <Show when={devices.loading}>
        <p aria-live="polite" class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>
          Loading devices…
        </p>
      </Show>
      <Show when={devices.error}>
        <p role="alert" class="md-body-m" style={{ margin: "0", color: "var(--md-sys-color-error)" }}>
          {devices.error instanceof Error ? devices.error.message : String(devices.error)}
        </p>
      </Show>
      <Show when={!devices.loading && !devices.error && (devices()?.devices.length ?? 0) === 0}>
        <EmptyState
          icon="devices"
          title="No browser devices"
          supporting="Pair a browser below, then refresh this list."
        />
      </Show>
      <Show when={!devices.error && (devices()?.devices.length ?? 0) > 0}>
        <List>
          <For each={devices()?.devices ?? []}>
            {(device) => (
              <ListRow
                leading={device.isSelf ? "devices" : "laptop_chromebook"}
                headline={
                  <span style={{ display: "inline-flex", "align-items": "baseline", gap: "var(--md-space-2)", "max-width": "100%" }}>
                    <span style={{ overflow: "hidden", "text-overflow": "ellipsis" }}>{device.label || "Unnamed browser"}</span>
                    <Show when={device.isSelf}>
                      <span class="md-label-s" style={{ color: "var(--md-sys-color-primary)", "flex-shrink": 0 }}>
                        This device
                      </span>
                    </Show>
                  </span>
                }
                support={
                  <span style={{ display: "block", "overflow-wrap": "anywhere" }}>
                    <span style={{ "font-family": "var(--term-font-family)" }}>{device.fingerprint}</span>
                    <span aria-hidden="true"> · </span>
                    <time dateTime={new Date(Number(device.addedAtMs)).toISOString()}>
                      {new Date(Number(device.addedAtMs)).toLocaleString()}
                    </time>
                  </span>
                }
                trailing={
                  <Show
                    when={!device.isSelf}
                    fallback={
                      <Show when={keyInfo()?.extractable}>
                        <Button
                          variant="tonal"
                          disabled={busyFingerprint() !== null}
                          onClick={() => void rotate()}
                        >
                          Upgrade key security
                        </Button>
                      </Show>
                    }
                  >
                    <Button
                      variant="tonal"
                      aria-label={`Revoke ${device.label || "unnamed browser"}`}
                      disabled={busyFingerprint() !== null}
                      onClick={() => void revoke(device.fingerprint, device.label)}
                    >
                      {busyFingerprint() === device.fingerprint ? "Revoking…" : "Revoke"}
                    </Button>
                  </Show>
                }
                testId={`authorized-device-${device.fingerprint}`}
              />
            )}
          </For>
        </List>
      </Show>
    </Card>
  );
}

export function DevicesPane() {
  return (
    <div data-testid="settings-devices-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <AuthorizedDevicesCard />
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

// Settings → Devices: self-hosted browser identities and pairing controls.
// The authorized-device card is shared with the managed Account pane, where
// the coordinator scopes DevicesList/Revoke to the signed-in account.

import { For, Show, createEffect, createResource, createSignal, onCleanup } from "solid-js";
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

interface AuthorizedDevicesCardProps {
  accountScoped?: boolean;
  disabled?: boolean;
  onBusyChange?: (busy: boolean) => void;
}

export function AuthorizedDevicesCard(props: AuthorizedDevicesCardProps) {
  const [devices, { refetch }] = createResource(() => coordClient.devicesList({}));
  const [keyInfo] = createResource(
    () => !props.accountScoped,
    async () => getCurrentWebKeyInfo(),
  );
  const [busyFingerprint, setBusyFingerprint] = createSignal<string | null>(null);

  createEffect(() => props.onBusyChange?.(busyFingerprint() !== null));
  onCleanup(() => props.onBusyChange?.(false));

  async function revoke(fingerprint: string, label: string): Promise<void> {
    const recovery = props.accountScoped ? "sign in again" : "pair again";
    const displayLabel = label || fingerprint.slice(0, 12);
    if (!confirm(`Revoke ${displayLabel}? This browser will need to ${recovery}.`)) return;

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
      title={props.accountScoped ? "Browser devices" : "Authorized devices"}
      supporting={props.accountScoped
        ? "Only browsers signed in to this account are listed. Revoking another browser signs it out permanently."
        : "Browsers authorized to access this coordinator. Revocation is permanent for that key."}
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
          supporting={props.accountScoped
            ? "Reconnect to the coordinator and refresh this page."
            : "Pair a browser below, then refresh this list."}
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
                        {props.accountScoped ? "This browser" : "This device"}
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
                      <Show when={!props.accountScoped && keyInfo()?.extractable}>
                        <Button
                          variant="tonal"
                          disabled={props.disabled || busyFingerprint() !== null}
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
                      disabled={props.disabled || busyFingerprint() !== null}
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

// Collapsed "Other pairing options" on the unauthorized pairing page: manual
// one-time setup-token redemption and, only after the coordinator rejects this
// device key, key recovery. PairingGatePanel mounts it; redemption goes through
// redeemPairToken and recovery through web-key.ts's locked resetWebKey().

import { createSignal, createUniqueId, Show } from "solid-js";
import type { JSX } from "solid-js";
import { isResetWebKeyEligible, resetWebKey } from "../../client/auth/web-key.ts";
import { redeemPairToken } from "../../store/auth/redeemPairToken.ts";
import { Button, Card, TextField } from "../Settings/md/primitives.tsx";
import { PairingStatusNotice } from "./PairingStatusNotice.tsx";
import type { PairingRequester } from "./PairingRequesterProvider.tsx";

const SETUP_TOKEN_PREFIX = "roost_bt_";

export function PairingOtherOptions(props: { requester: PairingRequester }): JSX.Element {
  const panelId = createUniqueId();
  const [open, setOpen] = createSignal(false);
  const [setupToken, setSetupToken] = createSignal("");
  const [redeemState, setRedeemState] = createSignal<"idle" | "redeeming" | "paired">("idle");
  const [redeemError, setRedeemError] = createSignal<string | null>(null);
  const [resetBusy, setResetBusy] = createSignal(false);
  const [resetError, setResetError] = createSignal<string | null>(null);
  // The rejection probe signs a coordinator request, so it runs once, on the
  // first expand, instead of on every unpaired page load.
  const [keyRecovery, setKeyRecovery] = createSignal<"unprobed" | "probing" | "offered" | "hidden">("unprobed");

  function toggleOptions(): void {
    setOpen((current) => !current);
    if (keyRecovery() !== "unprobed") return;
    setKeyRecovery("probing");
    isResetWebKeyEligible().then(
      (deviceRejected) => setKeyRecovery(deviceRejected ? "offered" : "hidden"),
      () => setKeyRecovery("hidden"),
    );
  }

  async function redeemSetupToken(): Promise<void> {
    if (redeemState() === "redeeming") return;
    setRedeemState("redeeming");
    setRedeemError(null);
    const result = await redeemPairToken(setupToken());
    if (!result.ok) {
      setRedeemState("idle");
      setRedeemError(result.error);
      return;
    }
    // A requester ceremony left behind would restart its recovery poll after
    // the redirect even though this browser is already paired.
    props.requester.clear();
    setRedeemState("paired");
    window.location.replace("/");
  }

  function redeemPastedSetupToken(event: ClipboardEvent): void {
    const pastedToken = event.clipboardData?.getData("text") ?? "";
    if (!pastedToken.startsWith(SETUP_TOKEN_PREFIX)) return;
    setSetupToken(pastedToken);
    setTimeout(() => void redeemSetupToken(), 0);
    event.preventDefault();
  }

  async function createNewDeviceKey(): Promise<void> {
    if (resetBusy()) return;
    if (!confirm("Create a new device key? This browser will need to pair again.")) return;
    setResetBusy(true);
    setResetError(null);
    props.requester.clear();
    try {
      await resetWebKey();
    } catch (error) {
      setResetError(`Key reset failed: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setResetBusy(false);
    }
  }

  return (
    <Card
      variant="outlined"
      title="Other pairing options"
      trailing={
        <Button
          variant="ghost"
          size="sm"
          icon={open() ? "expand_less" : "expand_more"}
          aria-expanded={open()}
          aria-controls={panelId}
          data-testid="pairing-other-options-toggle"
          onClick={toggleOptions}
        >
          {open() ? "Hide" : "Show"}
        </Button>
      }
    >
      <Show when={open()}>
        <div id={panelId} class="pairing-options__panel" data-testid="pairing-other-options-panel">
          <div class="pairing-options__section">
            <span class="md-title-s pairing-options__heading">Use a one-time setup token</span>
            <TextField
              type="text"
              testId="onboarding-setup-token-input"
              value={setupToken()}
              onInput={setSetupToken}
              placeholder={`${SETUP_TOKEN_PREFIX}…`}
              label="Setup token"
              autocomplete="off"
              ref={(element) => {
                if (element instanceof HTMLInputElement) element.onpaste = redeemPastedSetupToken;
              }}
            />
            <div>
              <Button
                variant="secondary"
                data-testid="onboarding-setup-token-submit"
                onClick={() => void redeemSetupToken()}
                disabled={!setupToken() || redeemState() !== "idle"}
              >
                {redeemState() === "redeeming" ? "Pairing…" : "Pair with token"}
              </Button>
            </div>
            <Show when={redeemError()}>
              {(message) => <PairingStatusNotice tone="error" message={`Redeem failed: ${message()}`} />}
            </Show>
            <Show when={redeemState() === "paired"}>
              <PairingStatusNotice tone="ok" message="Browser paired. Opening Roost…" />
            </Show>
          </div>
          <Show when={keyRecovery() === "offered"}>
            <div class="pairing-options__section" data-testid="pairing-key-recovery">
              <span class="md-title-s pairing-options__heading">Recover a rejected browser key</span>
              <p class="md-body-m pairing-options__supporting">
                If this browser was previously paired and revoked, create a new local
                device key before requesting access again.
              </p>
              <div>
                <Button
                  variant="secondary"
                  data-testid="onboarding-reset-key-btn"
                  disabled={resetBusy()}
                  onClick={() => void createNewDeviceKey()}
                >
                  Create new device key
                </Button>
              </div>
              <Show when={resetError()}>
                {(message) => <PairingStatusNotice tone="error" message={message()} />}
              </Show>
            </div>
          </Show>
        </div>
      </Show>
    </Card>
  );
}

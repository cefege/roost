// MachineDeployDialog — mint a bootstrap token and surface the deploy command.
// Shown when user clicks "Add Machine" in MachinesPane.
// Depends on: trpc (auth.mintBootstrap), Solid signals.
// Callers: MachinesPane.tsx.

import { createSignal, Show } from "solid-js";
import { coordClient, coordinatorBaseUrl } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { TextField, Button, Dialog, Select } from "./Settings/md/primitives.tsx";
import { workerCoordinatorUrl } from "../lib/workerCoordinatorUrl.ts";
import { browserPlatform } from "../lib/browserPlatform.ts";
import { buildMachineJoinCommand, machinePlatformLabel } from "@roost/shared/machine-join-command";
import { copyToClipboard } from "../lib/clipboard.ts";
import { supportedWorkerPlatform } from "../lib/nativePath.ts";
import type { SupportedHostPlatform } from "@roost/shared/platform";

interface MachineDeployDialogProps {
  onClose: () => void;
}



export function MachineDeployDialog(props: MachineDeployDialogProps) {
  const clientPlatform = browserPlatform();
  const [targetPlatform, setTargetPlatform] = createSignal<SupportedHostPlatform>(
    clientPlatform === "windows" ? "win32" : clientPlatform === "linux" ? "linux" : "darwin",
  );
  const [label, setLabel] = createSignal("");
  const [windowsPublisherSha256, setWindowsPublisherSha256] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [deployCmd, setDeployCmd] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal(false);
  let copyTimer: ReturnType<typeof setTimeout> | null = null;
  // The worker's dial URL and its one-shot token MUST come from the same
  // coordinator. rootStore holds the identity the same-origin bootstrap client
  // fetched, which is a different coordinator whenever a self-hosted override is
  // active, so the URL is resolved from the active client at mint time: the
  // coordinator's declared public URL when it has one, else the origin that
  // client talks to.

  async function mintAndShowCmd() {
    setLoading(true);
    setError("");
    setDeployCmd(null);
    try {
      const identity = await coordClient.authCoordIdentity({});
      const coordinatorUrl = workerCoordinatorUrl(identity.publicUrl, coordinatorBaseUrl());
      if (!coordinatorUrl) {
        setError(
          "Worker installation needs an HTTPS origin a worker can reach. Open Roost over your front door's HTTPS URL, or set ROOST_COORDINATOR_PUBLIC_URL on the coordinator when workers use a different door.",
        );
        return;
      }
      const lbl = label().trim();
      const publisher = targetPlatform() === "win32" ? windowsPublisherSha256().trim() : undefined;
      if (targetPlatform() === "win32" && !/^[0-9a-f]{64}$/i.test(publisher ?? "")) {
        setError("Windows enrollment requires the trusted 64-hex release-publisher SHA-256.");
        return;
      }
      const result = await coordClient.authMintBootstrap({ kind: "worker", label: lbl });
      setDeployCmd(buildMachineJoinCommand(targetPlatform(), coordinatorUrl, result.token, lbl, publisher));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  async function copyCmd() {
    const cmd = deployCmd();
    if (!cmd) return;
    // Clipboard denial stays quiet — the command is on screen for manual copy.
    if (!(await copyToClipboard(cmd))) return;
    setCopied(true);
    if (copyTimer !== null) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => { copyTimer = null; setCopied(false); }, 2000);
  }

  function mintOnFieldEnter(event: KeyboardEvent): void {
    if (event.defaultPrevented || event.key !== "Enter" || deployCmd()) return;
    event.preventDefault();
    void mintAndShowCmd();
  }


  return (
    <Dialog
      open
      onClose={props.onClose}
      headline="Add Machine"
      testId="machine-deploy-dialog"
      actions={rootStore.coord_identity
        ? (deployCmd()
          ? (
            <>
              <Button
                variant="secondary"
                data-testid="machine-deploy-copy"
                onClick={() => void copyCmd()}
                style={{ color: copied() ? "var(--color-ok)" : undefined }}
              >
                {copied() ? "Copied ✓" : "Copy command"}
              </Button>
              <Button variant="outline" onClick={props.onClose}>Done</Button>
            </>
          )
          : (
            <>
              <Button variant="outline" onClick={props.onClose}>Cancel</Button>
              <Button
                variant="default"
                data-testid="machine-deploy-mint"
                onClick={() => void mintAndShowCmd()}
                disabled={loading()}
              >
                {loading() ? "Minting…" : "Mint token"}
              </Button>
            </>
          ))
        : undefined}
      showCloseButton={!rootStore.coord_identity}
    >
      <div>

        <p style={{ "font-size": "var(--md-body-s-size)", color: "var(--text-lo)", "margin-bottom": "16px", "line-height": "1.5" }}>
          Mint a one-time bootstrap token for a new worker. Run the generated
          command on the target machine to register it with this coordinator.
          The token expires after 24 hours.
        </p>

        <Show
          when={rootStore.coord_identity}
          fallback={
            <div
              data-testid="machine-deploy-pending"
              style={{
                background: "var(--md-sys-color-surface-container)",
                border: "1px solid var(--md-sys-color-outline)",
                "border-radius": "var(--md-shape-sm)",
                padding: "16px",
                "font-size": "13px",
                "line-height": "1.5",
                color: "var(--text-lo)",
              }}
            >
              Roost is still confirming how this deployment accepts workers.
            </div>
          }
        >
        <Show when={!deployCmd()}>
          <div style={{ "margin-bottom": "12px" }}>
            <Select
              label="Operating system"
              value={targetPlatform()}
              onChange={(value) => setTargetPlatform(supportedWorkerPlatform(value) ?? "darwin")}
              testId="machine-deploy-platform"
              options={[
                { value: "darwin", label: "macOS" },
                { value: "linux", label: "Linux" },
                { value: "win32", label: "Windows 11 / Server 2022" },
              ]}
            />
          </div>
          <Show when={targetPlatform() === "win32"}>
            <div style={{ "margin-bottom": "12px" }}>
              <TextField
                testId="machine-deploy-windows-publisher"
                label="Trusted release-publisher SHA-256"
                value={windowsPublisherSha256()}
                onInput={(value) => setWindowsPublisherSha256(value)}
                onKeyDown={mintOnFieldEnter}
                placeholder="64 hexadecimal characters"
                style={{ width: "100%" }}
              />
              <p style={{ color: "var(--text-lo)", "font-size": "11px", "line-height": "1.4", margin: "6px 0 0" }}>
                Get this certificate fingerprint through the trusted release
                channel, not from the manifest or script being downloaded.
              </p>
            </div>
          </Show>
          <div style={{ "margin-bottom": "12px" }}>
            <TextField
              testId="machine-deploy-label"
              label="Machine label"
              value={label()}
              onInput={(value) => setLabel(value)}
              onKeyDown={mintOnFieldEnter}
              placeholder="optional — defaults to the machine's hostname"
              style={{ width: "100%" }}
            />
          </div>

          <Show when={error()}>
            <p style={{ color: "var(--color-err)", "font-size": "var(--md-body-s-size)", "margin-bottom": "10px" }}>{error()}</p>
          </Show>


        </Show>

        <Show when={deployCmd()}>
          <div>
            <div style={{ "font-size": "11px", color: "var(--text-lo)", "margin-bottom": "8px", "text-transform": "uppercase", "letter-spacing": "0.06em" }}>
              Run this on the new {machinePlatformLabel(targetPlatform())} (it must be able to reach the coordinator URL):
            </div>
            <div style={{
              background: "var(--bg-base)",
              border: "1px solid var(--md-sys-color-outline-variant)",
              "border-radius": "var(--md-shape-sm)",
              padding: "10px 12px",
              "font-family": "monospace",
              "font-size": "var(--md-body-s-size)",
              color: "var(--color-ok)",
              "word-break": "break-all" as const,
              "line-height": "1.6",
              "margin-bottom": "12px",
            }}>
              {deployCmd()}
            </div>

          </div>
        </Show>
        </Show>
      </div>
    </Dialog>
  );
}

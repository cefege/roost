// MachineDeployDialog — discovers the active coordinator's enrollment door and
// mints a one-use worker bootstrap command only after that door is reachable.
// MachinesPane mounts it on demand; identity and request lifetimes stay local
// so a closed dialog cannot receive late coordinator or clipboard results.

import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { workerCoordinatorUrl } from "@roost/protocol/coordinator-dial-url";
import { buildMachineJoinCommand } from "@roost/platform/machine-join-command";
import { coordClient, coordinatorBaseUrl } from "../connect.ts";
import { browserPlatform } from "../lib/browserPlatform.ts";
import { copyToClipboard } from "../lib/clipboard.ts";
import {
  Button,
  Dialog,
  Select,
  StatusDot,
  Surface,
  TextField,
} from "./Settings/md/primitives.tsx";
import { MachineLocalAccessGuide } from "./MachineLocalAccessGuide.tsx";

interface MachineDeployDialogProps {
  onClose: () => void;
}

type MachineDeployState = "checking" | "local-only" | "ready" | "generated" | "error";
type PosixTargetPlatform = "darwin" | "linux";

type DeploymentError = {
  kind: "configuration" | "rpc";
  title: string;
  detail: string;
};

type EnrollmentDecision =
  | { kind: "ready"; coordinatorUrl: string }
  | { kind: "local-only" }
  | { kind: "configuration-error" };

const STACK_STYLE = {
  display: "grid",
  gap: "var(--md-space-4)",
} as const;

const SUPPORT_STYLE = {
  margin: 0,
  color: "var(--md-sys-color-on-surface-variant)",
} as const;

const ENDPOINT_STYLE = {
  color: "var(--text-hi)",
  "overflow-wrap": "anywhere",
} as const;


function enrollmentDecision(publicUrl: string): EnrollmentDecision {
  const declaredUrl = publicUrl.trim();
  const coordinatorUrl = workerCoordinatorUrl(declaredUrl, coordinatorBaseUrl());
  if (coordinatorUrl) return { kind: "ready", coordinatorUrl };
  return declaredUrl ? { kind: "configuration-error" } : { kind: "local-only" };
}


export function MachineDeployDialog(props: MachineDeployDialogProps) {
  const clientPlatform = browserPlatform();
  const [dialogState, setDialogState] = createSignal<MachineDeployState>("checking");
  const [targetPlatform, setTargetPlatform] = createSignal<PosixTargetPlatform | null>(
    clientPlatform === "macos" ? "darwin" : clientPlatform === "linux" ? "linux" : null,
  );
  const [label, setLabel] = createSignal("");
  const [coordinatorUrl, setCoordinatorUrl] = createSignal<string | null>(null);
  const [deployCommand, setDeployCommand] = createSignal<string | null>(null);
  const [deploymentError, setDeploymentError] = createSignal<DeploymentError | null>(null);
  const [minting, setMinting] = createSignal(false);
  const [copied, setCopied] = createSignal(false);
  let copyTimer: number | undefined;
  let active = true;
  let operationVersion = 0;
  let mintOperation: number | null = null;

  function isCurrentOperation(version: number): boolean {
    return active && version === operationVersion;
  }

  function applyEnrollmentDecision(decision: EnrollmentDecision): void {
    switch (decision.kind) {
      case "ready":
        setCoordinatorUrl(decision.coordinatorUrl);
        setDeploymentError(null);
        setDialogState("ready");
        return;
      case "local-only":
        setCoordinatorUrl(null);
        setDeploymentError(null);
        setDialogState("local-only");
        return;
      case "configuration-error":
        setCoordinatorUrl(null);
        setDeploymentError({
          kind: "configuration",
          title: "The configured enrollment address is invalid.",
          detail: "Configure a reachable HTTPS address for this Roost, then check again. A declared address must not be a local-only URL.",
        });
        setDialogState("error");
        return;
    }
  }

  function reportRpcError(error: unknown, title: string): void {
    setCoordinatorUrl(null);
    setDeploymentError({
      kind: "rpc",
      title,
      detail: error instanceof Error && error.message
        ? error.message
        : "The coordinator did not return a usable response.",
    });
    setDialogState("error");
  }

  async function checkEnrollment(): Promise<void> {
    const version = ++operationVersion;
    setDialogState("checking");
    setCoordinatorUrl(null);
    setDeploymentError(null);
    try {
      const identity = await coordClient.authCoordIdentity({});
      if (!isCurrentOperation(version)) return;
      applyEnrollmentDecision(enrollmentDecision(identity.publicUrl));
    } catch (error) {
      if (!isCurrentOperation(version)) return;
      reportRpcError(error, "Roost could not check the coordinator.");
    }
  }

  async function generateJoinCommand(): Promise<void> {
    const selectedPlatform = targetPlatform();
    if (
      mintOperation !== null
      || dialogState() !== "ready"
      || !selectedPlatform
      || !coordinatorUrl()
    ) return;

    const selectedLabel = label().trim();
    const version = ++operationVersion;
    mintOperation = version;
    setMinting(true);
    setDeploymentError(null);
    try {
      const identity = await coordClient.authCoordIdentity({});
      if (!isCurrentOperation(version)) return;

      const decision = enrollmentDecision(identity.publicUrl);
      if (decision.kind !== "ready") {
        applyEnrollmentDecision(decision);
        return;
      }

      const result = await coordClient.authMintBootstrap({ kind: "worker", label: selectedLabel });
      if (!isCurrentOperation(version)) return;
      setCoordinatorUrl(decision.coordinatorUrl);
      setDeployCommand(
        buildMachineJoinCommand(selectedPlatform, decision.coordinatorUrl, result.token, selectedLabel),
      );
      setDialogState("generated");
    } catch (error) {
      if (!isCurrentOperation(version)) return;
      reportRpcError(error, "Roost could not generate the join command.");
    } finally {
      if (mintOperation === version) {
        mintOperation = null;
        if (isCurrentOperation(version)) setMinting(false);
      }
    }
  }

  async function copyCommand(): Promise<void> {
    const command = deployCommand();
    if (!command || !active || !(await copyToClipboard(command)) || !active) return;
    setCopied(true);
    window.clearTimeout(copyTimer);
    copyTimer = window.setTimeout(() => {
      copyTimer = undefined;
      if (active) setCopied(false);
    }, 2_000);
  }

  function generateOnFieldEnter(event: KeyboardEvent): void {
    if (
      event.defaultPrevented
      || event.key !== "Enter"
      || dialogState() !== "ready"
      || !targetPlatform()
      || minting()
    ) return;
    event.preventDefault();
    void generateJoinCommand();
  }

  function invalidatePendingOperations(): void {
    active = false;
    operationVersion += 1;
    window.clearTimeout(copyTimer);
    copyTimer = undefined;
  }

  function closeDialog(): void {
    invalidatePendingOperations();
    props.onClose();
  }

  onMount(() => { void checkEnrollment(); });
  onCleanup(invalidatePendingOperations);

  return (
    <Dialog
      open
      onClose={closeDialog}
      headline="Add Machine"
      testId="machine-deploy-dialog"
      showCloseButton
      actions={(
        <>
          <Show when={dialogState() === "generated"}>
            <Button
              variant="secondary"
              data-testid="machine-deploy-copy"
              onClick={() => void copyCommand()}
            >
              {copied() ? "Copied" : "Copy command"}
            </Button>
            <Button variant="outline" onClick={closeDialog}>Done</Button>
          </Show>
          <Show when={dialogState() !== "generated"}>
            <Button variant="outline" onClick={closeDialog}>Cancel</Button>
            <Show when={dialogState() === "ready"}>
              <span
                data-testid="machine-deploy-mint"
                onClick={(event) => {
                  if (event.target === event.currentTarget) void generateJoinCommand();
                }}
              >
                <Button
                  variant="default"
                  data-testid="machine-deploy-generate"
                  onClick={() => void generateJoinCommand()}
                  disabled={minting() || !targetPlatform()}
                >
                  {minting() ? "Generating…" : "Generate join command"}
                </Button>
              </span>
            </Show>
            <Show when={dialogState() === "error"}>
              <Button variant="secondary" onClick={() => void checkEnrollment()}>
                Check again
              </Button>
            </Show>
          </Show>
        </>
      )}
    >
      <div style={STACK_STYLE}>
        <Show when={dialogState() === "checking" || dialogState() === "local-only"}>
          <MachineLocalAccessGuide
            checking={dialogState() === "checking"}
            onRecheck={() => void checkEnrollment()}
          />
        </Show>

        <Show when={dialogState() === "ready" || dialogState() === "error"}>
          <div style={STACK_STYLE}>
            <Show when={dialogState() === "ready" && coordinatorUrl()}>
              {(url) => (
                <Surface level={1} radius="sm" pad={3} border>
                  <div style={STACK_STYLE}>
                    <p class="md-label-m" style={SUPPORT_STYLE}>Enrollment address</p>
                    <code class="md-body-m" style={ENDPOINT_STYLE}>{url()}</code>
                  </div>
                </Surface>
              )}
            </Show>

            <Select
              label="Operating system"
              value={targetPlatform() ?? ""}
              onChange={(value) => setTargetPlatform(
                value === "darwin" || value === "linux" ? value : null,
              )}
              testId="machine-deploy-platform"
              disabled={minting()}
              placeholder="Choose an operating system"
              options={[
                { value: "darwin", label: "macOS" },
                { value: "linux", label: "Linux" },
              ]}
            />
            <p class="md-body-s" style={SUPPORT_STYLE}>
              Windows host releases are paused. Choose macOS or Linux for the target machine.
            </p>

            <TextField
              testId="machine-deploy-label"
              label="Machine label"
              value={label()}
              onInput={setLabel}
              onKeyDown={generateOnFieldEnter}
              placeholder="optional — defaults to the machine's hostname"
              disabled={minting()}
            />

            <Show when={deploymentError()}>
              {(error) => (
                <Surface
                  level={1}
                  radius="sm"
                  pad={3}
                  border
                  role="alert"
                  aria-live="polite"
                  aria-atomic="true"
                >
                  <div style={{ display: "flex", "align-items": "flex-start", gap: "var(--md-space-2)" }}>
                    <StatusDot status="error" />
                    <div style={STACK_STYLE} data-machine-deploy-error-kind={error().kind}>
                      <p class="md-title-s" style={{ margin: 0, color: "var(--md-sys-color-error)" }}>
                        {error().title}
                      </p>
                      <p class="md-body-s" style={SUPPORT_STYLE}>{error().detail}</p>
                    </div>
                  </div>
                </Surface>
              )}
            </Show>
          </div>
        </Show>

        <Show when={dialogState() === "generated" && deployCommand()}>
          {(command) => (
            <div style={STACK_STYLE}>
              <p class="md-body-m" style={SUPPORT_STYLE}>
                Run this command on the target using your normal terminal, SSH session, or cloud console. The target
                must reach <code style={ENDPOINT_STYLE}>{coordinatorUrl()}</code>.
              </p>
              <Surface level={1} radius="sm" pad={3} border>
                <code class="md-body-s" style={ENDPOINT_STYLE}>{command()}</code>
              </Surface>
              <p class="md-body-m" style={SUPPORT_STYLE}>
                This one-use command expires after 24 hours. Treat this command as a secret. Run it only on the
                machine you want to add.
              </p>
            </div>
          )}
        </Show>
      </div>
    </Dialog>
  );
}

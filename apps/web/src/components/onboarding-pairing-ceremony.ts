// Requester pairing owns tab-scoped capabilities, idempotent creation, polling,
// and confirmation recovery. Onboarding only renders these signals; no secret
// reaches rootStore, Sync, cross-tab storage, or a URL.

import { Code, ConnectError } from "@connectrpc/connect";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairRequestId,
  generatePairRequesterToken,
  normalizePairVerificationCode,
} from "@roost/shared/pairing";
import { backoffDelayMs } from "@roost/shared/retry";
import { createSignal, onCleanup, onMount } from "solid-js";
import { getPublicKeyB64 } from "../auth/web-key.ts";
import {
  clearPairingCeremony,
  compactPairVerificationCode,
  loadPairingCeremony,
  savePairingCeremony,
} from "../auth/pairing-ceremony.ts";
import type { PairingCeremony } from "../auth/pairing-ceremony.ts";
import { coordClient } from "../connect.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";
import { addToast } from "../store/toastStore.ts";
import type { PairPollStatus } from "./OnboardingRequestCard.tsx";

const POLL_INTERVAL_MS = 5_000;
const RETRY_BASE_MS = 1_000;
const RETRY_MAX_MS = 30_000;

type PairingAction = "create" | "poll" | "recovery_poll";

type PairingOperation = {
  generation: number;
  ceremony: PairingCeremony;
  restored: boolean;
  createAcknowledged: boolean;
  createInFlight: boolean;
  pollInFlight: boolean;
  confirmInFlight: boolean;
  confirmationMayHaveCommitted: boolean;
  confirmationRecoveryPending: boolean;
  retryAttempt: number;
  publicKey: string | null;
  requesterLabel: string | null;
};

export function createOnboardingPairingCeremony(callbacks: {
  redirectAfterPairing: () => void;
  reportRequestError: (message: string) => void;
}) {
  const restored = loadPairingCeremony();
  const [ephemeralId, setEphemeralId] = createSignal<string | null>(
    restored?.ephemeralId ?? null,
  );
  const [pollStatus, setPollStatus] = createSignal<PairPollStatus>(
    restored ? "pending" : "idle",
  );
  const [verificationCode, setVerificationCode] = createSignal("");
  const [confirmationError, setConfirmationError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  let active = true;
  let operationGeneration = 0;
  let currentOperation: PairingOperation | null = null;
  let scheduledAction: Parameters<typeof clearTimeout>[0] | null = null;
  function cancelScheduledAction(): void {
    if (scheduledAction === null) return;
    clearTimeout(scheduledAction);
    scheduledAction = null;
  }
  function isCurrent(operation: PairingOperation): boolean {
    return active
      && currentOperation === operation
      && operation.generation === operationGeneration;
  }
  function retireCurrentOperation(): void {
    cancelScheduledAction();
    currentOperation = null;
    operationGeneration += 1;
  }
  function beginOperation(ceremony: PairingCeremony, restored = false): PairingOperation {
    retireCurrentOperation();
    const operation: PairingOperation = {
      generation: ++operationGeneration,
      ceremony,
      restored,
      createAcknowledged: false,
      createInFlight: false,
      pollInFlight: false,
      confirmInFlight: false,
      confirmationMayHaveCommitted: false,
      confirmationRecoveryPending: false,
      retryAttempt: 0,
      publicKey: null,
      requesterLabel: null,
    };
    currentOperation = operation;
    savePairingCeremony(ceremony);
    setEphemeralId(ceremony.ephemeralId);
    setPollStatus("pending");
    setVerificationCode("");
    setConfirmationError(null);
    setBusy(false);
    return operation;
  }
  function schedule(operation: PairingOperation, action: PairingAction, delayMs: number): void {
    if (!isCurrent(operation)) return;
    cancelScheduledAction();
    scheduledAction = setTimeout(() => {
      scheduledAction = null;
      if (!isCurrent(operation)) return;
      if (action === "create") void submitCreate(operation);
      else void poll(operation, action === "recovery_poll");
    }, Math.max(0, Math.floor(delayMs)));
  }
  function retryDelay(operation: PairingOperation, error: unknown): number {
    const attempt = operation.retryAttempt++;
    const fallback = backoffDelayMs(attempt, {
      baseMs: RETRY_BASE_MS,
      maxMs: RETRY_MAX_MS,
    });
    if (!(error instanceof ConnectError)) return fallback;
    const retryAfterSeconds = Number(error.metadata.get("retry-after"));
    if (!Number.isFinite(retryAfterSeconds) || retryAfterSeconds <= 0) return fallback;
    return Math.min(RETRY_MAX_MS, Math.ceil(retryAfterSeconds * 1_000));
  }
  function terminalFailure(operation: PairingOperation, message: string): void {
    if (!isCurrent(operation)) return;
    retireCurrentOperation();
    clearPairingCeremony();
    setPollStatus("error");
    setVerificationCode("");
    setConfirmationError(message);
    setBusy(false);
    callbacks.reportRequestError(message);
    addToast(message, "err");
  }
  function finishTerminalPoll(operation: PairingOperation, status: Extract<PairPollStatus, "denied" | "expired" | "verification_failed">): void {
    if (!isCurrent(operation)) return;
    retireCurrentOperation();
    clearPairingCeremony();
    setPollStatus(status);
    setVerificationCode("");
    setConfirmationError(null);
    setBusy(false);
    const message = status === "verification_failed"
      ? "Pair verification failed — request again"
      : `Pair request ${status}`;
    addToast(message, "warn");
  }
  function finishCompleted(operation: PairingOperation): void {
    if (!isCurrent(operation)) return;
    retireCurrentOperation();
    clearPairingCeremony();
    setEphemeralId(null);
    setPollStatus("completed");
    setVerificationCode("");
    setConfirmationError(null);
    setBusy(false);
    addToast("Browser verified — opening home", "ok");
    callbacks.redirectAfterPairing();
  }
  async function submitCreate(operation: PairingOperation): Promise<void> {
    if (!isCurrent(operation) || operation.createInFlight) return;
    operation.createInFlight = true;
    setBusy(true);
    let createRetryDelay: number | null = null;
    let createRecoveryPoll = false;
    try {
      let publicKey = operation.publicKey;
      let requesterLabel = operation.requesterLabel;
      if (publicKey === null) {
        try {
          publicKey = await getPublicKeyB64();
          requesterLabel = browserSelfLabel();
          operation.publicKey = publicKey;
          operation.requesterLabel = requesterLabel;
        } catch (error) {
          if (!isCurrent(operation)) return;
          terminalFailure(
            operation,
            `Could not prepare browser pairing: ${pairingErrorMessage(error)}`,
          );
          return;
        }
      }
      if (!isCurrent(operation) || publicKey === null) return;
      const response = await coordClient.pairCreate({
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId: operation.ceremony.ephemeralId,
        requesterToken: operation.ceremony.requesterToken,
        sshPubkeyB64: publicKey,
        label: requesterLabel ?? browserSelfLabel(),
      });
      if (!isCurrent(operation)) return;
      if (response.ephemeralId !== operation.ceremony.ephemeralId) {
        terminalFailure(operation, "Pairing request did not match this browser.");
        return;
      }
      operation.createAcknowledged = true;
      operation.retryAttempt = 0;
      setPollStatus("pending");
    } catch (error) {
      if (!isCurrent(operation)) return;
      if (isRetryablePairingError(error)) createRetryDelay = retryDelay(operation, error);
      else if (
        operation.restored
        && error instanceof ConnectError
        && error.code === Code.FailedPrecondition
      ) {
        operation.createAcknowledged = true;
        createRecoveryPoll = true;
      } else terminalFailure(operation, `Pair create failed: ${pairingErrorMessage(error)}`);
    } finally {
      if (!isCurrent(operation)) return;
      operation.createInFlight = false;
      setBusy(false);
      if (createRetryDelay !== null) schedule(operation, "create", createRetryDelay);
      else if (createRecoveryPoll) schedule(operation, "recovery_poll", 0);
      else if (operation.createAcknowledged) schedule(operation, "poll", POLL_INTERVAL_MS);
    }
  }
  async function poll(operation: PairingOperation, confirmationRecovery = false): Promise<void> {
    if (!isCurrent(operation) || !operation.createAcknowledged || operation.confirmInFlight) return;
    if (operation.pollInFlight) {
      if (confirmationRecovery) schedule(operation, "recovery_poll", POLL_INTERVAL_MS);
      return;
    }
    operation.pollInFlight = true;
    let pollRetryDelay: number | null = null;
    let nextPoll = false;
    try {
      const response = await coordClient.pairPoll({
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId: operation.ceremony.ephemeralId,
        requesterToken: operation.ceremony.requesterToken,
      });
      if (!isCurrent(operation) || (!confirmationRecovery && operation.confirmationRecoveryPending)) return;
      operation.retryAttempt = 0;
      switch (response.status) {
        case "pending":
        case "verification_required":
          setPollStatus(response.status);
          setConfirmationError(null);
          if (confirmationRecovery) {
            operation.confirmationRecoveryPending = false;
            setBusy(false);
          }
          nextPoll = true;
          break;
        case "denied":
        case "expired":
        case "verification_failed":
          finishTerminalPoll(operation, response.status);
          break;
        case "completed":
          finishCompleted(operation);
          break;
        default:
          terminalFailure(operation, "Pair poll returned an unknown status.");
      }
    } catch (error) {
      if (!isCurrent(operation) || (!confirmationRecovery && operation.confirmationRecoveryPending)) return;
      if (isRetryablePairingError(error)) pollRetryDelay = retryDelay(operation, error);
      else terminalFailure(operation, `Pair poll failed: ${pairingErrorMessage(error)}`);
    } finally {
      if (!isCurrent(operation)) return;
      operation.pollInFlight = false;
      if (pollRetryDelay !== null) {
        schedule(operation, confirmationRecovery ? "recovery_poll" : "poll", pollRetryDelay);
      }
      else if (nextPoll) schedule(operation, "poll", POLL_INTERVAL_MS);
    }
  }
  async function start(): Promise<void> {
    let ceremony: PairingCeremony;
    try {
      ceremony = {
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId: generatePairRequestId(),
        requesterToken: generatePairRequesterToken(),
      };
    } catch (error) {
      const message = `Could not start browser pairing: ${pairingErrorMessage(error)}`;
      setPollStatus("error");
      setConfirmationError(message);
      callbacks.reportRequestError(message);
      addToast(message, "err");
      return;
    }
    const operation = beginOperation(ceremony);
    await submitCreate(operation);
  }
  function clear(): void {
    retireCurrentOperation();
    clearPairingCeremony();
    setEphemeralId(null);
    setPollStatus("idle");
    setVerificationCode("");
    setConfirmationError(null);
    setBusy(false);
  }
  function updateVerificationCode(value: string): void {
    setVerificationCode(value);
    setConfirmationError(null);
  }
  async function confirm(): Promise<void> {
    const operation = currentOperation;
    if (
      operation === null
      || !isCurrent(operation)
      || operation.confirmInFlight
      || operation.confirmationRecoveryPending
      || pollStatus() !== "verification_required"
      || busy()
    ) return;
    const normalizedVerificationCode = normalizePairVerificationCode(
      compactPairVerificationCode(verificationCode()),
    );
    if (normalizedVerificationCode === null) {
      setConfirmationError("Enter the six-digit code from the paired browser.");
      return;
    }
    cancelScheduledAction();
    operation.confirmInFlight = true;
    setBusy(true);
    let pollRetryDelay: number | null = null;
    try {
      const response = await coordClient.pairConfirm({
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId: operation.ceremony.ephemeralId,
        requesterToken: operation.ceremony.requesterToken,
        verificationCode: normalizedVerificationCode,
      });
      if (!isCurrent(operation)) return;
      operation.retryAttempt = 0;
      if (!response.ok) {
        setConfirmationError("That code did not match. Check it and try again.");
        return;
      }
      finishCompleted(operation);
    } catch (error) {
      if (!isCurrent(operation)) return;
      if (isRetryablePairingError(error)) {
        setConfirmationError("Confirmation interrupted. Checking pairing status.");
        pollRetryDelay = retryDelay(operation, error);
        operation.confirmationMayHaveCommitted = true;
        operation.confirmationRecoveryPending = true;
      } else if (operation.confirmationMayHaveCommitted) {
        setConfirmationError("Checking pairing status.");
        pollRetryDelay = 0;
        operation.confirmationRecoveryPending = true;
      } else terminalFailure(operation, `Confirmation failed: ${pairingErrorMessage(error)}`);
    } finally {
      if (!isCurrent(operation)) return;
      operation.confirmInFlight = false;
      setBusy(pollRetryDelay !== null);
      schedule(operation, pollRetryDelay === null ? "poll" : "recovery_poll", pollRetryDelay ?? POLL_INTERVAL_MS);
    }
  }
  onMount(() => {
    if (restored === null || currentOperation !== null) return;
    const operation = beginOperation(restored, true);
    void submitCreate(operation);
  });
  onCleanup(() => {
    active = false;
    cancelScheduledAction();
    currentOperation = null;
    operationGeneration += 1;
  });
  return {
    busy,
    clear,
    confirm,
    confirmationError,
    ephemeralId,
    pollStatus,
    start,
    updateVerificationCode,
    verificationCode,
  };
}
function isRetryablePairingError(error: unknown): boolean {
  if (!(error instanceof ConnectError)) return true;
  return error.code === Code.Unknown
    || error.code === Code.Unavailable
    || error.code === Code.DeadlineExceeded
    || error.code === Code.Aborted
    || error.code === Code.ResourceExhausted;
}
function pairingErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

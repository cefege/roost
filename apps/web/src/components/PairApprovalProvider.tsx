// PairApprovalProvider is the sole approver-code owner for every pairing UI.
// It persists one tab-scoped idempotent approval, renders the only code dialog
// once PairApprove acknowledges the generated code, polls PairApprovalStatus
// until the requester confirms, and turns every dialog dismissal into PairDeny.
// Evidence gathering, failure classification, and outcome copy live in
// auth/pair-approval-lifecycle.ts; this component owns timers, fences, and UI.

import { diag } from "@roost/shared/diag";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairVerificationCode,
  normalizePairRequestId,
} from "@roost/shared/pairing";
import {
  createContext,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  useContext,
} from "solid-js";
import type { JSX } from "solid-js";
import {
  PAIR_APPROVAL_OUTCOME_TOASTS,
  PAIR_APPROVE_PATH,
  isRetryablePairApprovalError,
  pairApprovalRetryDelayMs,
  requestApprovalStatus,
  requestPairDenial,
  resolveApprovalStatus,
  resolveCancellation,
} from "../auth/pair-approval-lifecycle.ts";
import type { PairApprovalOutcome, PairApprovalStep } from "../auth/pair-approval-lifecycle.ts";
import {
  clearPairApproval,
  loadPairApproval,
  savePairApproval,
} from "../auth/pairing-approval.ts";
import type { PairApprovalRecord } from "../auth/pairing-approval.ts";
import { coordClient } from "../connect.ts";
import { announcePairedBrowser } from "../lib/pairedBrowserNotice.ts";
import { deletePairRequest } from "../store/mutations.ts";
import { addToast } from "../store/toastStore.ts";
import { PairVerificationCodeDialog } from "./PairVerificationCodeDialog.tsx";
import type { PairCodeDialogState } from "./PairVerificationCodeDialog.tsx";

const STATUS_POLL_INTERVAL_MS = 1_000;

export interface PairApprovalRequest {
  ephemeralId: string;
  requesterLabel: string;
  expiresAtMs: number;
}

export interface PairApprovalContextValue {
  busyRequestId: () => string | null;
  approve(request: PairApprovalRequest): Promise<void>;
}

type ApprovalPhase = "approving" | "awaiting_confirmation" | "cancelling";

// Every phase transition installs a fresh operation, so an RPC or timer that
// captured an earlier phase's object is fenced out by identity and generation.
type ApprovalOperation = {
  generation: number;
  phase: ApprovalPhase;
  record: PairApprovalRecord;
  inFlight: boolean;
  retryAttempt: number;
  // Cancelling sends PairDeny first; after its NotFound only status reads decide.
  cancelRequest: "deny" | "status";
};

const PairApprovalContext = createContext<PairApprovalContextValue>();

export function usePairApproval(): PairApprovalContextValue {
  const context = useContext(PairApprovalContext);
  if (context === undefined) throw new Error("Pair approval provider is unavailable.");
  return context;
}

export function PairApprovalProvider(props: {
  enabled: boolean;
  children?: JSX.Element;
}): JSX.Element {
  const [busyRequestId, setBusyRequestId] = createSignal<string | null>(null);
  const [dialogApproval, setDialogApproval] = createSignal<PairApprovalRecord | null>(null);
  const [dialogState, setDialogState] = createSignal<PairCodeDialogState>("awaiting");
  let active = true;
  let operationGeneration = 0;
  let currentOperation: ApprovalOperation | null = null;
  let stepTimer: Parameters<typeof clearTimeout>[0] | null = null;
  let expiryTimer: Parameters<typeof clearTimeout>[0] | null = null;

  function cancelStepTimer(): void {
    if (stepTimer === null) return;
    clearTimeout(stepTimer);
    stepTimer = null;
  }

  function cancelExpiry(): void {
    if (expiryTimer === null) return;
    clearTimeout(expiryTimer);
    expiryTimer = null;
  }

  function isCurrent(operation: ApprovalOperation): boolean {
    return active
      && props.enabled
      && currentOperation === operation
      && operation.generation === operationGeneration;
  }

  function retireCurrentOperation(): void {
    cancelStepTimer();
    cancelExpiry();
    currentOperation = null;
    operationGeneration += 1;
  }

  function clearApprovalState(): void {
    retireCurrentOperation();
    clearPairApproval();
    setBusyRequestId(null);
    setDialogApproval(null);
  }

  function installOperation(record: PairApprovalRecord, phase: ApprovalPhase): ApprovalOperation {
    cancelStepTimer();
    const operation: ApprovalOperation = {
      generation: ++operationGeneration,
      phase,
      record,
      inFlight: false,
      retryAttempt: 0,
      cancelRequest: "deny",
    };
    currentOperation = operation;
    diag("pair.approval_phase", { phase, ephemeral_id: record.ephemeralId });
    return operation;
  }

  function beginApproval(record: PairApprovalRecord): ApprovalOperation {
    retireCurrentOperation();
    const operation = installOperation(record, "approving");
    savePairApproval(record);
    setBusyRequestId(record.ephemeralId);
    setDialogApproval(null);
    armExpiry(operation);
    return operation;
  }

  // Only an unacknowledged approval expires on the local clock. Once the code
  // is shown, the coordinator's status read owns expiry, so a skewed clock
  // never hides a code the requester can still use.
  function armExpiry(operation: ApprovalOperation): void {
    cancelExpiry();
    const delayMs = Math.max(0, operation.record.expiresAtMs - Date.now());
    expiryTimer = setTimeout(() => {
      expiryTimer = null;
      if (isCurrent(operation)) settle(operation, "expired");
    }, delayMs);
  }

  function scheduleStep(
    operation: ApprovalOperation,
    delayMs: number,
    step: (operation: ApprovalOperation) => Promise<void>,
  ): void {
    if (!isCurrent(operation)) return;
    cancelStepTimer();
    stepTimer = setTimeout(() => {
      stepTimer = null;
      if (isCurrent(operation)) void step(operation);
    }, Math.max(0, Math.floor(delayMs)));
  }

  async function submitApproval(operation: ApprovalOperation): Promise<void> {
    if (!isCurrent(operation) || operation.inFlight) return;
    operation.inFlight = true;
    setBusyRequestId(operation.record.ephemeralId);
    try {
      const response = await coordClient.pairApprove({
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId: operation.record.ephemeralId,
        verificationCode: operation.record.verificationCode,
      });
      if (!isCurrent(operation)) return;
      operation.inFlight = false;
      if (response.ok) {
        awaitConfirmation(operation);
        return;
      }
      clearApprovalState();
      addToast("Pair approval was rejected.", "err");
    } catch (error) {
      if (!isCurrent(operation)) return;
      operation.inFlight = false;
      if (isRetryablePairApprovalError(error, PAIR_APPROVE_PATH)) {
        scheduleStep(operation, pairApprovalRetryDelayMs(operation.retryAttempt++, error), submitApproval);
        if (operation.retryAttempt === 1) addToast("Pair approval is retrying.", "warn");
        return;
      }
      clearApprovalState();
      const message = error instanceof Error ? error.message : String(error);
      addToast(`Approve failed: ${message}`, "err");
    }
  }

  function awaitConfirmation(approving: ApprovalOperation): void {
    cancelExpiry();
    const awaiting = installOperation(approving.record, "awaiting_confirmation");
    setDialogState("awaiting");
    setDialogApproval(awaiting.record);
    deletePairRequest(awaiting.record.ephemeralId);
    addToast("Verification code ready.", "ok");
    scheduleStep(awaiting, STATUS_POLL_INTERVAL_MS, readApprovalStatus);
  }

  async function readApprovalStatus(operation: ApprovalOperation): Promise<void> {
    if (!isCurrent(operation) || operation.inFlight) return;
    operation.inFlight = true;
    const evidence = await requestApprovalStatus(operation.record.ephemeralId);
    if (!isCurrent(operation)) return;
    operation.inFlight = false;
    applyStep(operation, resolveApprovalStatus(evidence));
  }

  async function submitCancellation(operation: ApprovalOperation): Promise<void> {
    if (!isCurrent(operation) || operation.inFlight) return;
    operation.inFlight = true;
    const evidence = operation.cancelRequest === "deny"
      ? await requestPairDenial(operation.record.ephemeralId)
      : await requestApprovalStatus(operation.record.ephemeralId);
    if (!isCurrent(operation)) return;
    operation.inFlight = false;
    applyStep(operation, resolveCancellation(evidence));
  }

  function applyStep(operation: ApprovalOperation, step: PairApprovalStep): void {
    const repeat = operation.phase === "cancelling" ? submitCancellation : readApprovalStatus;
    switch (step.kind) {
      case "poll":
        operation.retryAttempt = 0;
        scheduleStep(operation, STATUS_POLL_INTERVAL_MS, readApprovalStatus);
        return;
      case "retry":
        scheduleStep(operation, pairApprovalRetryDelayMs(operation.retryAttempt++, step.error), repeat);
        return;
      case "read_status":
        operation.cancelRequest = "status";
        operation.retryAttempt = 0;
        void submitCancellation(operation);
        return;
      case "send_deny":
        operation.cancelRequest = "deny";
        scheduleStep(operation, pairApprovalRetryDelayMs(operation.retryAttempt++, null), repeat);
        return;
      case "settle":
        settle(operation, step.outcome);
    }
  }

  function settle(operation: ApprovalOperation, outcome: PairApprovalOutcome): void {
    const { ephemeralId, requesterLabel } = operation.record;
    diag("pair.approval_settled", { outcome, phase: operation.phase, ephemeral_id: ephemeralId });
    if (outcome === "reload") {
      // The code stays valid for the requester; only this client lost the
      // ability to follow the ceremony, so the dialog keeps it and asks for a
      // reload while Cancel request stays available.
      if (operation.phase === "cancelling") installOperation(operation.record, "awaiting_confirmation");
      setDialogState("reload_required");
      return;
    }
    clearApprovalState();
    if (outcome === "completed") {
      announcePairedBrowser({ ephemeralId, label: requesterLabel });
      return;
    }
    const toast = PAIR_APPROVAL_OUTCOME_TOASTS[outcome];
    addToast(toast.message, toast.kind);
  }

  function cancelApproval(): void {
    const operation = currentOperation;
    if (operation === null || !isCurrent(operation) || operation.phase !== "awaiting_confirmation") return;
    // The persisted record exists only to replay PairApprove after a reload;
    // once cancellation begins, approval must never be replayed.
    clearPairApproval();
    setDialogState("cancelling");
    void submitCancellation(installOperation(operation.record, "cancelling"));
  }

  async function approve(request: PairApprovalRequest): Promise<void> {
    if (!active || !props.enabled || currentOperation !== null) return;
    const ephemeralId = normalizePairRequestId(request.ephemeralId);
    const expiresAtMs = Math.trunc(request.expiresAtMs);
    if (
      ephemeralId === null
      || !Number.isSafeInteger(expiresAtMs)
      || expiresAtMs <= Date.now()
    ) {
      addToast(PAIR_APPROVAL_OUTCOME_TOASTS.expired.message, PAIR_APPROVAL_OUTCOME_TOASTS.expired.kind);
      return;
    }
    const operation = beginApproval({
      ceremonyVersion: PAIRING_CEREMONY_VERSION,
      ephemeralId,
      verificationCode: generatePairVerificationCode(),
      requesterLabel: request.requesterLabel,
      expiresAtMs,
    });
    await submitApproval(operation);
  }

  createEffect(() => {
    if (!props.enabled) {
      if (currentOperation !== null || dialogApproval() !== null) {
        retireCurrentOperation();
        setBusyRequestId(null);
        setDialogApproval(null);
      }
      return;
    }
    if (!active || dialogApproval() !== null) return;
    let operation = currentOperation;
    if (operation === null) {
      const restored = loadPairApproval();
      if (restored === null) return;
      if (restored.expiresAtMs <= Date.now()) {
        clearPairApproval();
        return;
      }
      operation = beginApproval(restored);
    }
    if (!operation.inFlight && stepTimer === null) void submitApproval(operation);
  });

  onCleanup(() => {
    active = false;
    cancelStepTimer();
    cancelExpiry();
    currentOperation = null;
    operationGeneration += 1;
  });

  const context: PairApprovalContextValue = { busyRequestId, approve };
  // Keyed on the approval record alone: lifecycle state reaches the mounted
  // dialog through its props instead of remounting it and its focus trap.
  const codeDialog = createMemo(() => {
    const approval = dialogApproval();
    if (approval === null) return null;
    return (
      <PairVerificationCodeDialog
        open
        verificationCode={approval.verificationCode}
        requesterLabel={approval.requesterLabel}
        state={dialogState()}
        onCancel={cancelApproval}
        onReload={() => location.reload()}
      />
    );
  });
  return (
    <PairApprovalContext.Provider value={context}>
      {props.children}
      {codeDialog()}
    </PairApprovalContext.Provider>
  );
}

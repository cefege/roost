// PairApprovalProvider is the sole approver-code owner for every pairing UI.
// It persists one tab-scoped idempotent approval and renders the only code
// dialog after PairApprove acknowledges the exact generated code.

import { Code, ConnectError } from "@connectrpc/connect";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairVerificationCode,
  normalizePairRequestId,
} from "@roost/shared/pairing";
import { backoffDelayMs } from "@roost/shared/retry";
import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  Show,
  useContext,
} from "solid-js";
import type { JSX } from "solid-js";
import {
  clearPairApproval,
  loadPairApproval,
  savePairApproval,
} from "../auth/pairing-approval.ts";
import type { PairApprovalRecord } from "../auth/pairing-approval.ts";
import { coordClient } from "../connect.ts";
import { deletePairRequest } from "../store/mutations.ts";
import { addToast } from "../store/toastStore.ts";
import { PairVerificationCodeDialog } from "./PairVerificationCodeDialog.tsx";

const RETRY_BASE_MS = 1_000;
const RETRY_MAX_MS = 30_000;

export interface PairApprovalRequest {
  ephemeralId: string;
  requesterLabel: string;
  expiresAtMs: number;
}

export interface PairApprovalContextValue {
  busyRequestId: () => string | null;
  approve(request: PairApprovalRequest): Promise<void>;
}

type ApprovalOperation = {
  generation: number;
  record: PairApprovalRecord;
  inFlight: boolean;
  retryAttempt: number;
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
  let active = true;
  let operationGeneration = 0;
  let currentOperation: ApprovalOperation | null = null;
  let retryTimer: Parameters<typeof clearTimeout>[0] | null = null;
  let expiryTimer: Parameters<typeof clearTimeout>[0] | null = null;

  function cancelRetry(): void {
    if (retryTimer === null) return;
    clearTimeout(retryTimer);
    retryTimer = null;
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
    cancelRetry();
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

  function beginApproval(record: PairApprovalRecord): ApprovalOperation {
    retireCurrentOperation();
    const operation: ApprovalOperation = {
      generation: ++operationGeneration,
      record,
      inFlight: false,
      retryAttempt: 0,
    };
    currentOperation = operation;
    savePairApproval(record);
    setBusyRequestId(record.ephemeralId);
    setDialogApproval(null);
    armExpiry(operation);
    return operation;
  }

  function armExpiry(operation: ApprovalOperation): void {
    cancelExpiry();
    const delayMs = Math.max(0, operation.record.expiresAtMs - Date.now());
    expiryTimer = setTimeout(() => {
      expiryTimer = null;
      if (!isCurrent(operation)) return;
      clearApprovalState();
      addToast("Pair request expired.", "warn");
    }, delayMs);
  }

  function scheduleRetry(operation: ApprovalOperation, delayMs: number): void {
    if (!isCurrent(operation)) return;
    cancelRetry();
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (isCurrent(operation)) void submitApproval(operation);
    }, Math.max(0, Math.floor(delayMs)));
  }

  function retryDelay(operation: ApprovalOperation, error: unknown): number {
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

  function dismissDialog(): void {
    if (dialogApproval() === null) return;
    clearApprovalState();
  }

  async function submitApproval(operation: ApprovalOperation): Promise<void> {
    if (!isCurrent(operation) || operation.inFlight) return;
    operation.inFlight = true;
    setBusyRequestId(operation.record.ephemeralId);
    let approvalRetryDelay: number | null = null;
    try {
      const response = await coordClient.pairApprove({
        ceremonyVersion: PAIRING_CEREMONY_VERSION,
        ephemeralId: operation.record.ephemeralId,
        verificationCode: operation.record.verificationCode,
      });
      if (!isCurrent(operation)) return;
      if (!response.ok) {
        clearApprovalState();
        addToast("Pair approval was rejected.", "err");
        return;
      }
      operation.retryAttempt = 0;
      setDialogApproval(operation.record);
      deletePairRequest(operation.record.ephemeralId);
      addToast("Verification code ready.", "ok");
    } catch (error) {
      if (!isCurrent(operation)) return;
      const retryable = !(error instanceof ConnectError)
        || error.code === Code.Unknown
        || error.code === Code.Unavailable
        || error.code === Code.DeadlineExceeded
        || error.code === Code.Aborted
        || error.code === Code.ResourceExhausted;
      if (retryable) {
        approvalRetryDelay = retryDelay(operation, error);
        if (operation.retryAttempt === 1) addToast("Pair approval is retrying.", "warn");
      } else {
        clearApprovalState();
        const message = error instanceof Error ? error.message : String(error);
        addToast(`Approve failed: ${message}`, "err");
      }
    } finally {
      if (!isCurrent(operation)) return;
      operation.inFlight = false;
      if (approvalRetryDelay !== null) scheduleRetry(operation, approvalRetryDelay);
    }
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
      addToast("Pair request expired.", "warn");
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
    if (!operation.inFlight && retryTimer === null) void submitApproval(operation);
  });

  onCleanup(() => {
    active = false;
    cancelRetry();
    cancelExpiry();
    currentOperation = null;
    operationGeneration += 1;
  });

  const context: PairApprovalContextValue = { busyRequestId, approve };
  return (
    <PairApprovalContext.Provider value={context}>
      {props.children}
      <Show when={dialogApproval()}>
        {(approval) => (
          <PairVerificationCodeDialog
            open
            verificationCode={approval().verificationCode}
            requesterLabel={approval().requesterLabel}
            onClose={dismissDialog}
          />
        )}
      </Show>
    </PairApprovalContext.Provider>
  );
}

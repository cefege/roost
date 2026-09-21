// Owns coordinator-authoritative machine update presentation, request actions,
// and durable report display. MachineCard supplies a worker projection; this
// module never derives state from browser output streams or local deploy state.
// Depends on the shared operation contract, update RPC reader, M3 primitives,
// clipboard helper, refresh projection, and ToastCard's details surface.

import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import type { Worker } from "@roost/shared/wire";
import type { WorkerUpdateOperation, WorkerUpdateReport } from "@roost/shared/worker-update-operation";
import { addToast } from "../../store/toastStore.ts";
import { copyToClipboard } from "../../lib/clipboard.ts";
import { refreshCoordAndWorkers } from "../../store/sync-bootstrap.ts";
import { Button } from "./md/primitives.tsx";
import {
  machineUpdateStartPending,
  readMachineUpdateReport,
  machineUpdateReportRefreshRevision,
  startMachineUpdateDeploy,
} from "./machine-update-deploy.ts";

export type MachineUpdatePresentationState =
  | "unknown"
  | "up-to-date"
  | "pending-offline"
  | "available"
  | "updating"
  | "confirming"
  | "blocked"
  | "failed";

export type MachineUpdatePresentation = {
  readonly state: MachineUpdatePresentationState;
  readonly label: string;
  readonly detail: string | null;
  readonly action: "update" | "retry" | null;
  readonly actionDisabled: boolean;
  readonly expectedGitSha: string | null;
  readonly operation: WorkerUpdateOperation | null;
};

export type MachineUpdatePresentationInput = {
  readonly workerGitSha: string | null;
  readonly coordinatorGitSha: string | null;
  readonly online: boolean;
  readonly operation: WorkerUpdateOperation | null;
};

export type MachineUpdateDetailsProps = {
  worker: Pick<Worker, "fp" | "label" | "reachable_addr">;
  presentation: MachineUpdatePresentation;
};

const UPDATE_PHASE_LABELS: Record<WorkerUpdateOperation["phase"], string> = {
  preflight: "Preflight",
  recovery: "Recovery",
  staging: "Staging",
  activation: "Activation",
  confirmation: "Confirmation",
  settled: "Settled",
};

const UPDATE_REASON_DETAILS: Record<Exclude<WorkerUpdateOperation["reasonCode"], null>, string> = {
  offline: "This machine is offline. Updates automatically when this machine returns.",
  busy: "Another update owns this machine.",
  source_unavailable: "The coordinator source release is unavailable.",
  runtime_unavailable: "The installed worker runtime is unavailable.",
  keeper_incompatible: "The live keeper runtime is incompatible with the target release.",
  keeper_unproven: "The live keeper could not prove safe session preservation.",
  journal_conflict: "A host recovery journal conflicts with this update.",
  coordinator_restarting: "The coordinator is restarting and cannot admit this update yet.",
  confirmation_pending: "The update is waiting for worker confirmation.",
  confirmation_timeout: "The worker confirmation timed out.",
  deploy_failed: "The deployment owner reported a failed activation.",
  report_unavailable: "The coordinator could not retrieve the update report.",
  unsupported_platform: "This platform does not support worker updates.",
};

/** Derive one machine row from the shared worker projection. An unsettled
 * operation deliberately wins over matching versions until its owner settles. */
export function deriveMachineUpdatePresentation(
  input: MachineUpdatePresentationInput,
): MachineUpdatePresentation {
  const operation = input.operation;
  const operationTargetsCurrent = operation !== null
    && (input.coordinatorGitSha === null || operation.targetGitSha === input.coordinatorGitSha);

  if (operation && (
    operation.status === "queued"
    || operation.status === "running"
    || operation.status === "waiting"
    || operation.status === "verifying"
  )) {
    if (operation.status === "verifying" || operation.reasonCode === "confirmation_pending") {
      return {
        state: "confirming",
        label: "Waiting for worker confirmation",
        detail: `Waiting for worker confirmation. Target ${operation.targetGitSha}.`,
        action: "update",
        actionDisabled: true,
        expectedGitSha: input.coordinatorGitSha,
        operation,
      };
    }
    const waiting = operation.status === "waiting";
    return {
      state: "updating",
      label: waiting
        ? `Waiting — ${UPDATE_PHASE_LABELS[operation.phase]}`
        : `${operation.status === "queued" ? "Queued" : "Updating"} — ${UPDATE_PHASE_LABELS[operation.phase]}`,
      detail: `Current stage: ${UPDATE_PHASE_LABELS[operation.phase]}. Target ${operation.targetGitSha}.${operation.message ? ` ${operation.message}` : ""}`,
      action: "update",
      actionDisabled: true,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  if (operation && operation.status === "blocked" && operationTargetsCurrent) {
    const reason = operation.reasonCode
      ? UPDATE_REASON_DETAILS[operation.reasonCode]
      : "The coordinator blocked this update before it changed the machine.";
    return {
      state: "blocked",
      label: "Update blocked",
      detail: `${operation.message ? `${operation.message} ` : ""}${reason} Retry requests a fresh safe preflight.`,
      action: input.coordinatorGitSha === null ? null : "retry",
      actionDisabled: false,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  if (operation && operation.status === "failed" && operationTargetsCurrent) {
    if (input.online) {
      const reason = operation.reasonCode
        ? UPDATE_REASON_DETAILS[operation.reasonCode]
        : "The coordinator recorded a failed update.";
      return {
        state: "failed",
        label: "Update failed",
        detail: `${operation.message ? `${operation.message} ` : ""}${reason}`,
        action: input.coordinatorGitSha === null ? null : "retry",
        actionDisabled: false,
        expectedGitSha: input.coordinatorGitSha,
        operation,
      };
    }
    return {
      state: "pending-offline",
      label: "Update pending — offline",
      detail: "Updates automatically when this machine returns.",
      action: null,
      actionDisabled: false,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  if (operation && operation.status === "succeeded" && operationTargetsCurrent
    && input.workerGitSha !== input.coordinatorGitSha) {
    return {
      state: "confirming",
      label: "Waiting for worker confirmation",
      detail: `Waiting for worker confirmation. Target ${operation.targetGitSha}.`,
      action: "update",
      actionDisabled: true,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  if (!input.workerGitSha || !input.coordinatorGitSha) {
    return {
      state: "unknown",
      label: "Version unknown",
      detail: null,
      action: null,
      actionDisabled: false,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  if (input.workerGitSha === input.coordinatorGitSha) {
    return {
      state: "up-to-date",
      label: "Up to date",
      detail: null,
      action: null,
      actionDisabled: false,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  if (!input.online) {
    return {
      state: "pending-offline",
      label: "Update pending — offline",
      detail: "Updates automatically when this machine returns.",
      action: null,
      actionDisabled: false,
      expectedGitSha: input.coordinatorGitSha,
      operation,
    };
  }

  return {
    state: "available",
    label: "Update available",
    detail: null,
    action: "update",
    actionDisabled: false,
    expectedGitSha: input.coordinatorGitSha,
    operation,
  };
}

/** Stable JSON for the report region, clipboard, and ToastCard details. */
export function formatMachineUpdateReport(
  worker: MachineUpdateDetailsProps["worker"],
  report: WorkerUpdateReport,
): string {
  return JSON.stringify({
    schemaVersion: report.schemaVersion,
    machine: {
      label: worker.label,
      fingerprint: worker.fp,
      host: report.operation.host,
      reachableAddress: worker.reachable_addr,
    },
    coordinatorOrigin: report.coordinatorOrigin,
    operation: report.operation,
    observedGitSha: report.observedGitSha,
    failure: report.failure,
    events: report.events,
  }, null, 2);
}

export function MachineUpdateDetails(props: MachineUpdateDetailsProps) {
  const [submissionError, setSubmissionError] = createSignal("");
  const [report, setReport] = createSignal<WorkerUpdateReport | null>(null);
  const [reportLoading, setReportLoading] = createSignal(false);
  const [reportUnavailable, setReportUnavailable] = createSignal(false);
  const [copyStatus, setCopyStatus] = createSignal("");
  let reportRequest = 0;
  let failureToastJobId: string | null = null;
  let reportAbort: AbortController | null = null;
  let reportOperationKey: string | null | undefined;

  createEffect(() => {
    const operation = props.presentation.operation;
    const refreshRevision = machineUpdateReportRefreshRevision();
    const nextReportOperationKey = operation
      ? `${operation.jobId}:${operation.revision}:${refreshRevision}`
      : null;
    if (nextReportOperationKey === reportOperationKey) return;
    reportOperationKey = nextReportOperationKey;
    const request = ++reportRequest;
    reportAbort?.abort();
    setCopyStatus("");
    if (operation) setSubmissionError("");
    if (!operation) {
      reportAbort = null;
      setReport(null);
      setReportLoading(false);
      setReportUnavailable(false);
      return;
    }
    const abort = new AbortController();
    reportAbort = abort;
    const priorReport = report();
    if (!priorReport || priorReport.operation.jobId !== operation.jobId) setReport(null);
    setReportLoading(true);
    setReportUnavailable(false);
    void readMachineUpdateReport(operation.jobId, abort.signal).then((result) => {
      if (request !== reportRequest) return;
      setReportLoading(false);
      if (
        result.kind !== "available"
        || result.report.operation.jobId !== operation.jobId
        || result.report.operation.workerFp !== props.worker.fp
      ) {
        setReportUnavailable(true);
        return;
      }
      setReport(result.report);
      if (result.report.operation.status === "failed" && failureToastJobId !== operation.jobId) {
        failureToastJobId = operation.jobId;
        addToast("Machine update failed", "err", {
          details: formatMachineUpdateReport(props.worker, result.report),
        });
      }
    });
  });

  onCleanup(() => {
    reportRequest++;
    reportAbort?.abort();
  });

  async function startUpdate() {
    const expectedGitSha = props.presentation.expectedGitSha;
    if (!expectedGitSha) {
      setSubmissionError("Coordinator release is unknown; reconnect and retry.");
      return;
    }
    setSubmissionError("");
    const failure = await startMachineUpdateDeploy(props.worker.fp, expectedGitSha);
    if (failure) {
      setSubmissionError(failure);
      addToast("Update request failed", "err", { details: failure });
      return;
    }
    void refreshCoordAndWorkers();
  }

  async function copyReport() {
    const updateReport = report();
    if (!updateReport) return;
    const text = formatMachineUpdateReport(props.worker, updateReport);
    if (await copyToClipboard(text)) {
      setCopyStatus("Copied");
      return;
    }
    setCopyStatus("Copying failed");
    addToast("Copying update report failed", "err", { details: text });
  }

  const reportText = () => {
    const updateReport = report();
    return updateReport ? formatMachineUpdateReport(props.worker, updateReport) : "";
  };

  return (
    <Show when={props.presentation.detail || props.presentation.action || props.presentation.operation}>
      <div class="machines-worker-details__actions">
        <Show when={props.presentation.detail}>
          <span class="md-body-s" data-testid={`machines-update-summary-${props.worker.fp}`}>
            {props.presentation.detail}
          </span>
        </Show>
        <Show when={props.presentation.action}>
          <Button
            variant="default"
            icon="system_update_alt"
            data-testid={`machines-update-btn-${props.worker.fp}`}
            onClick={() => void startUpdate()}
            disabled={props.presentation.actionDisabled || machineUpdateStartPending(props.worker.fp)}
          >
            {machineUpdateStartPending(props.worker.fp) ? "Requesting…" : props.presentation.action === "retry" ? "Retry" : "Update"}
          </Button>
        </Show>
        <Show when={submissionError()}>
          <span class="md-body-s" data-testid={`machines-update-error-${props.worker.fp}`}>
            {submissionError()}
          </span>
        </Show>
        <Show when={props.presentation.operation}>
          {(operation) => (
            <Show when={props.presentation.expectedGitSha
              && operation().targetGitSha !== props.presentation.expectedGitSha}>
              <span class="md-body-s" data-testid={`machines-update-previous-${props.worker.fp}`}>
                Previous update targeted {operation().targetGitSha}; current coordinator target is {props.presentation.expectedGitSha}.
              </span>
            </Show>
          )}
        </Show>
        <Show when={props.presentation.operation && reportLoading()}>
          <span class="md-body-s" data-testid={`machines-update-report-loading-${props.worker.fp}`}>
            Loading update report…
          </span>
        </Show>
        <Show when={props.presentation.operation && reportUnavailable()}>
          <span class="md-body-s" data-testid={`machines-update-report-unavailable-${props.worker.fp}`}>
            Report unavailable
          </span>
        </Show>
        <Show when={report()}>
          <pre class="md-body-s machines-update-report" data-testid={`machines-update-report-${props.worker.fp}`}>
            {reportText()}
          </pre>
          <Button
            variant="ghost"
            data-testid={`machines-copy-update-report-${props.worker.fp}`}
            onClick={() => void copyReport()}
          >
            Copy update report
          </Button>
          <Show when={copyStatus()}>
            <span class="md-body-s" data-testid={`machines-update-report-copy-status-${props.worker.fp}`}>
              {copyStatus()}
            </span>
          </Show>
        </Show>
      </div>
    </Show>
  );
}

// Owns typed coordinator-side route-control correlation for browser Sync sockets.
// Sync terminal controls reserve one entry before a worker frame is sent; the
// worker frame dispatcher supplies the exact authenticated WorkerHandle here.
// Browser nonces are restored only after every worker and result fence passes.

import { create } from "@bufbuild/protobuf";
import {
  TerminalInputRouteResultSchema,
  TerminalTransportProbeResultSchema,
  type TerminalInputRouteResult,
  type TerminalTransportProbeResult,
} from "@roost/shared/proto/sync_pb";
import {
  type WTerminalInputRouteResult,
  type WTerminalTransportProbeResult,
} from "@roost/shared/proto/worker_transport_pb";
import {
  cancelPendingRpc,
  resolvePendingRpc,
} from "../router/pending-rpcs.ts";
import type { WorkerHandle } from "./worker-registry.ts";
import {
  isCurrentTerminalInputRouteWorker,
  sendTerminalInputRouteClaimRequest,
  sendTerminalTransportProbeRequest,
  type PendingTerminalRouteWorkerRequest,
} from "./worker-send-terminal-route.ts";
import { TerminalInputRouteRetirements } from "./terminal-input-route-retirements.ts";
import {
  terminalRouteConnectionNonceKey,
  type PendingTerminalRouteControl,
  type TerminalRouteControlSlot,
} from "./terminal-input-route-result-state.ts";
import {
  MAX_TERMINAL_INPUT_ROUTE_REVISION,
  MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET,
  TerminalRouteControlRefusal,
  isTerminalRouteIdentifier,
  isValidTerminalInputRouteResult,
  isValidTerminalTransportProbeResult,
  type TerminalInputRouteClaimRequest,
  type TerminalTransportProbeRequest,
} from "./terminal-input-route-result-contract.ts";

export {
  MAX_TERMINAL_INPUT_ROUTE_REVISION,
  MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET,
  TERMINAL_ROUTE_IDENTIFIER_MAX_UTF8_BYTES,
  TerminalRouteControlRefusal,
  isTerminalRouteIdentifier,
} from "./terminal-input-route-result-contract.ts";
export type {
  TerminalInputRouteClaimRequest,
  TerminalRouteControlRefusalReason,
  TerminalTransportProbeRequest,
} from "./terminal-input-route-result-contract.ts";


/** Composition-owned typed-result sink shared by worker and Sync transports. */
export class TerminalInputRouteResults {
  readonly #pendingByOuterRequestId = new Map<string, PendingTerminalRouteControl>();
  readonly #slotsByConnection = new Map<string, Set<TerminalRouteControlSlot>>();
  readonly #slotsByConnectionNonce = new Map<string, TerminalRouteControlSlot>();
  readonly #workersByConnection = new Map<string, Map<string, string>>();
  readonly #retirements = new TerminalInputRouteRetirements();
  #disposed = false;

  async claim(
    request: TerminalInputRouteClaimRequest,
    reservedSlot?: TerminalRouteControlSlot,
  ): Promise<TerminalInputRouteResult> {
    const slot = this.requireSlot(request.connectionId, request.browserRequestId, reservedSlot);
    try {
      this.assertClaimRequest(request);
      const pending = this.bindSlot(slot, {
        kind: "claim",
        worker: request.worker,
        workerFp: request.worker.workerFp,
        workerEpoch: request.workerEpoch,
        connectionGeneration: request.worker.connectionGeneration,
        sessionId: request.sessionId,
        revision: request.revision,
        outerRequestId: null,
      });
      this.trackWorker(slot.connectionId, request.worker, request.workerEpoch);
      const workerRequest = sendTerminalInputRouteClaimRequest(
        request.worker,
        {
          sessionId: request.sessionId,
          deviceFingerprint: request.deviceFingerprint,
          tabId: request.tabId,
          browserConnectionId: request.connectionId,
          revision: request.revision,
          workerEpoch: request.workerEpoch,
        },
        (created) => this.installWorkerRequest(pending, created),
        () => this.hasSlot(slot) && slot.pending === pending,
        request.deadline,
      );
      if (!workerRequest.admitted) {
        void workerRequest.result.catch(() => undefined);
        throw new TerminalRouteControlRefusal("terminal_input_route_unavailable");
      }
      const workerResult = await workerRequest.result;
      if (!this.isCurrentPendingWorker(pending, request.worker)) {
        throw new Error("terminal input route worker changed before reply");
      }
      if (!isValidTerminalInputRouteResult(pending, workerResult)) {
        throw new Error("terminal input route worker returned an invalid result");
      }
      const result = workerResult.result!;
      return create(TerminalInputRouteResultSchema, {
        ...result,
        requestId: pending.slot.browserRequestId,
      });
    } finally {
      this.releaseControl(slot);
    }
  }

  async probe(
    request: TerminalTransportProbeRequest,
    reservedSlot?: TerminalRouteControlSlot,
  ): Promise<TerminalTransportProbeResult> {
    const slot = this.requireSlot(request.connectionId, request.browserRequestId, reservedSlot);
    try {
      this.assertProbeRequest(request);
      const pending = this.bindSlot(slot, {
        kind: "probe",
        worker: request.worker,
        workerFp: request.workerFp,
        workerEpoch: request.workerEpoch,
        connectionGeneration: request.worker.connectionGeneration,
        sessionId: null,
        revision: null,
        outerRequestId: null,
      });
      this.trackWorker(slot.connectionId, request.worker, request.workerEpoch);
      const workerRequest = sendTerminalTransportProbeRequest(
        request.worker,
        { workerEpoch: request.workerEpoch },
        (created) => this.installWorkerRequest(pending, created),
        () => this.hasSlot(slot) && slot.pending === pending,
        request.deadline,
      );
      if (!workerRequest.admitted) {
        void workerRequest.result.catch(() => undefined);
        throw new TerminalRouteControlRefusal("terminal_transport_probe_unavailable");
      }
      const workerResult = await workerRequest.result;
      if (!this.isCurrentPendingWorker(pending, request.worker)) {
        throw new Error("terminal transport probe worker changed before reply");
      }
      if (!isValidTerminalTransportProbeResult(pending, workerResult)) {
        throw new Error("terminal transport probe worker returned an invalid result");
      }
      return create(TerminalTransportProbeResultSchema, {
        requestId: pending.slot.browserRequestId,
        workerFp: pending.workerFp,
        workerEpoch: pending.workerEpoch,
      });
    } finally {
      this.releaseControl(slot);
    }
  }

  /** Reserves one bounded control slot before any asynchronous route lookup. */
  reserveControl(connectionId: string, browserRequestId: string): TerminalRouteControlSlot {
    if (
      this.#disposed
      || !isTerminalRouteIdentifier(connectionId)
      || !isTerminalRouteIdentifier(browserRequestId)
    ) throw new TerminalRouteControlRefusal("terminal_input_route_unavailable");
    const slots = this.#slotsByConnection.get(connectionId) ?? new Set<TerminalRouteControlSlot>();
    if (slots.size >= MAX_TERMINAL_ROUTE_CONTROLS_PER_SOCKET) {
      throw new TerminalRouteControlRefusal("route_claim_busy");
    }
    const nonceKey = terminalRouteConnectionNonceKey(connectionId, browserRequestId);
    if (this.#slotsByConnectionNonce.has(nonceKey)) {
      throw new TerminalRouteControlRefusal("route_claim_busy");
    }
    const slot: TerminalRouteControlSlot = {
      connectionId,
      browserRequestId,
      pending: null,
    };
    slots.add(slot);
    this.#slotsByConnection.set(connectionId, slots);
    this.#slotsByConnectionNonce.set(nonceKey, slot);
    return slot;
  }

  /** Releases an unused admission or a completed typed result. */
  releaseControl(slot: TerminalRouteControlSlot): void {
    if (!this.hasSlot(slot)) return;
    if (slot.pending !== null) this.removeOuterRequest(slot.pending);
    slot.pending = null;
    const slots = this.#slotsByConnection.get(slot.connectionId);
    if (slots?.delete(slot) && slots.size === 0) {
      this.#slotsByConnection.delete(slot.connectionId);
    }
    const nonceKey = terminalRouteConnectionNonceKey(slot.connectionId, slot.browserRequestId);
    if (this.#slotsByConnectionNonce.get(nonceKey) === slot) {
      this.#slotsByConnectionNonce.delete(nonceKey);
    }
  }

  /** Cancels pending controls and retires all worker routes ever admitted for a Sync socket. */
  retireBrowserConnection(connectionId: string): void {
    const slots = [...(this.#slotsByConnection.get(connectionId) ?? [])];
    for (const slot of slots) {
      const pending = slot.pending;
      const outerRequestId = pending?.outerRequestId;
      const workerFp = pending?.workerFp;
      this.releaseControl(slot);
      if (outerRequestId !== null && outerRequestId !== undefined && workerFp) {
        cancelPendingRpc(outerRequestId, workerFp);
      }
    }
    const workers = this.#workersByConnection.get(connectionId);
    this.#workersByConnection.delete(connectionId);
    if (!workers) return;
    for (const [workerFp, workerEpoch] of workers) {
      this.#retirements.retire(workerFp, workerEpoch, connectionId);
    }
  }

  /** Flushes a close notice retained while this exact worker epoch was unroutable. */
  flushWorkerRetirements(workerFp: string): void {
    this.#retirements.flush(workerFp);
  }

  /** Called only by the current-generation worker frame dispatcher. */
  acceptInputRouteResult(source: WorkerHandle, frame: WTerminalInputRouteResult): boolean {
    const pending = this.#pendingByOuterRequestId.get(frame.requestId);
    if (
      !pending
      || pending.kind !== "claim"
      || !this.isCurrentPendingWorker(pending, source)
      || !isValidTerminalInputRouteResult(pending, frame)
    ) return false;
    this.removeOuterRequest(pending);
    return resolvePendingRpc(frame.requestId, frame, pending.workerFp);
  }

  /** Called only by the current-generation worker frame dispatcher. */
  acceptTransportProbeResult(source: WorkerHandle, frame: WTerminalTransportProbeResult): boolean {
    const pending = this.#pendingByOuterRequestId.get(frame.requestId);
    if (
      !pending
      || pending.kind !== "probe"
      || !this.isCurrentPendingWorker(pending, source)
      || !isValidTerminalTransportProbeResult(pending, frame)
    ) return false;
    this.removeOuterRequest(pending);
    return resolvePendingRpc(frame.requestId, frame, pending.workerFp);
  }


  /** Fences only waiters captured against this exact worker connection object. */
  cancelForWorkerHandle(worker: WorkerHandle, _reason: string): void {
    for (const slots of this.#slotsByConnection.values()) {
      for (const slot of [...slots]) {
        const pending = slot.pending;
        if (pending?.worker === worker) this.cancel(pending);
      }
    }

  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    for (const connectionId of [...this.#slotsByConnection.keys(), ...this.#workersByConnection.keys()]) {
      this.retireBrowserConnection(connectionId);
    }
  }

  private requireSlot(
    connectionId: string,
    browserRequestId: string,
    reservedSlot: TerminalRouteControlSlot | undefined,
  ): TerminalRouteControlSlot {
    if (!reservedSlot) return this.reserveControl(connectionId, browserRequestId);
    if (
      !this.hasSlot(reservedSlot)
      || reservedSlot.connectionId !== connectionId
      || reservedSlot.browserRequestId !== browserRequestId
    ) throw new TerminalRouteControlRefusal("terminal_input_route_unavailable");
    return reservedSlot;
  }

  private bindSlot(
    slot: TerminalRouteControlSlot,
    values: Omit<PendingTerminalRouteControl, "slot">,
  ): PendingTerminalRouteControl {
    if (!this.hasSlot(slot) || slot.pending !== null) {
      throw new TerminalRouteControlRefusal("terminal_input_route_unavailable");
    }
    const pending: PendingTerminalRouteControl = { ...values, slot };
    slot.pending = pending;
    return pending;
  }

  private trackWorker(connectionId: string, worker: WorkerHandle, workerEpoch: string): void {
    const workers = this.#workersByConnection.get(connectionId) ?? new Map<string, string>();
    workers.set(worker.workerFp, workerEpoch);
    this.#workersByConnection.set(connectionId, workers);
  }


  private installWorkerRequest(
    pending: PendingTerminalRouteControl,
    created: PendingTerminalRouteWorkerRequest<unknown>,
  ): void {
    if (
      !this.hasSlot(pending.slot)
      || pending.slot.pending !== pending
      || pending.outerRequestId !== null
      || created.requestId === pending.slot.browserRequestId
    ) {
      throw new Error("terminal route control is no longer pending");
    }
    if (this.#pendingByOuterRequestId.has(created.requestId)) {
      throw new Error("terminal route control request is already pending");
    }
    pending.outerRequestId = created.requestId;
    this.#pendingByOuterRequestId.set(created.requestId, pending);
  }

  private isCurrentPendingWorker(
    pending: PendingTerminalRouteControl,
    source: WorkerHandle,
  ): boolean {
    return source === pending.worker
      && source.connectionGeneration === pending.connectionGeneration
      && source.workerFp === pending.workerFp
      && isCurrentTerminalInputRouteWorker(source, pending.workerEpoch);
  }


  private assertClaimRequest(request: TerminalInputRouteClaimRequest): void {
    if (
      !isTerminalRouteIdentifier(request.browserRequestId)
      || !isTerminalRouteIdentifier(request.sessionId)
      || !isTerminalRouteIdentifier(request.deviceFingerprint)
      || !isTerminalRouteIdentifier(request.tabId)
      || !isTerminalRouteIdentifier(request.connectionId)
      || !isTerminalRouteIdentifier(request.workerEpoch)
      || request.revision <= 0n
      || request.revision > MAX_TERMINAL_INPUT_ROUTE_REVISION
      || !isCurrentTerminalInputRouteWorker(request.worker, request.workerEpoch)
    ) throw new TerminalRouteControlRefusal("terminal_input_route_unavailable");
  }

  private assertProbeRequest(request: TerminalTransportProbeRequest): void {
    if (
      !isTerminalRouteIdentifier(request.browserRequestId)
      || !isTerminalRouteIdentifier(request.connectionId)
      || !isTerminalRouteIdentifier(request.workerFp)
      || !isTerminalRouteIdentifier(request.workerEpoch)
      || request.workerFp !== request.worker.workerFp
      || !isCurrentTerminalInputRouteWorker(request.worker, request.workerEpoch)
    ) throw new TerminalRouteControlRefusal("terminal_transport_probe_unavailable");
  }

  private cancel(pending: PendingTerminalRouteControl): void {
    const outerRequestId = pending.outerRequestId;
    this.releaseControl(pending.slot);
    if (outerRequestId !== null) {
      cancelPendingRpc(outerRequestId, pending.workerFp);
    }
  }

  private removeOuterRequest(pending: PendingTerminalRouteControl): void {
    if (
      pending.outerRequestId !== null
      && this.#pendingByOuterRequestId.get(pending.outerRequestId) === pending
    ) {
      this.#pendingByOuterRequestId.delete(pending.outerRequestId);
    }
  }

  private hasSlot(slot: TerminalRouteControlSlot): boolean {
    return this.#slotsByConnection.get(slot.connectionId)?.has(slot) === true;
  }
}




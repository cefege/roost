// Owns bounded same-process recovery for committed worker events whose live
// publication lost a connection-generation race. Event transactions reserve
// entries before commit; exact dedupe replay consumes the retained effect.
// Worker credential deletion clears every reservation for that fingerprint.

import { asWorkspaceId, type SessionEvent, type WorkerFp } from "@roost/shared/wire";
import { sessionBus, workspaceBus, type SessionBusMessage } from "./buses.ts";
import { applyDurableChannelIndex } from "./byte-hub.ts";

export const PENDING_EVENT_PUBLICATION_MAX_ENTRIES = 256;

export interface CommittedEventPublication {
  readonly event: SessionEvent;
  readonly authenticatedWorkerFp: WorkerFp | null;
  readonly dashboardId: string;
  readonly eventId: number;
  readonly eventJson: string;
  readonly cascadeOrphanIds: readonly string[];
  readonly snapshotReapIds: readonly string[];
}

interface PendingPublicationSlot {
  readonly token: object;
  readonly settled: Promise<void>;
  readonly settle: () => void;
  state: "reserved" | "retained" | "claimed";
  effect?: CommittedEventPublication;
}

export interface PendingEventPublicationReservation {
  readonly workerFp: string;
  readonly clientSeq: number;
  readonly ownsReservation: boolean;
  readonly token: object;
  readonly settled: Promise<void>;
}

export type PendingEventPublicationClaim =
  | { readonly kind: "none" | "mismatch" }
  | {
      readonly kind: "claimed";
      readonly effect: CommittedEventPublication;
    };

export interface ResolveEventPublicationOptions {
  readonly store?: PendingEventPublicationStore;
  readonly reservation?: PendingEventPublicationReservation;
  readonly committedEffect?: CommittedEventPublication;
  readonly deduplicated: boolean;
  readonly canPublish?: () => boolean;
  readonly replayEventJson: string;
}

export interface EventPublicationResolution {
  readonly publishedEffect?: CommittedEventPublication;
  readonly replayRejected: boolean;
}

export class PendingEventPublicationStore {
  private readonly byWorker = new Map<
    string,
    Map<number, PendingPublicationSlot>
  >();
  private retainedEntries = 0;

  constructor(
    readonly capacity = PENDING_EVENT_PUBLICATION_MAX_ENTRIES,
  ) {
    if (!Number.isSafeInteger(capacity) || capacity <= 0) {
      throw new RangeError("pending event publication capacity must be a positive safe integer");
    }
  }

  get size(): number {
    return this.retainedEntries;
  }

  reserve(
    workerFp: string,
    clientSeq: number,
  ): PendingEventPublicationReservation {
    const existing = this.byWorker.get(workerFp)?.get(clientSeq);
    if (existing) {
      return {
        workerFp,
        clientSeq,
        ownsReservation: false,
        token: existing.token,
        settled: existing.settled,
      };
    }
    if (this.retainedEntries >= this.capacity) {
      throw new Error("pending event publication capacity exceeded");
    }

    const gate = Promise.withResolvers<void>();
    const slot: PendingPublicationSlot = {
      token: {},
      settled: gate.promise,
      settle: gate.resolve,
      state: "reserved",
    };
    let workerSlots = this.byWorker.get(workerFp);
    if (!workerSlots) {
      workerSlots = new Map();
      this.byWorker.set(workerFp, workerSlots);
    }
    workerSlots.set(clientSeq, slot);
    this.retainedEntries += 1;
    return {
      workerFp,
      clientSeq,
      ownsReservation: true,
      token: slot.token,
      settled: slot.settled,
    };
  }

  retain(
    reservation: PendingEventPublicationReservation,
    effect: CommittedEventPublication,
  ): boolean {
    if (!reservation.ownsReservation) return false;
    const slot = this.currentSlot(reservation);
    if (slot?.state !== "reserved") return false;
    slot.effect = effect;
    slot.state = "retained";
    slot.settle();
    return true;
  }

  release(reservation: PendingEventPublicationReservation): void {
    if (reservation.ownsReservation) this.removeCurrent(reservation);
  }

  async claim(
    reservation: PendingEventPublicationReservation,
    replayEventJson: string,
  ): Promise<PendingEventPublicationClaim> {
    if (!reservation.ownsReservation) await reservation.settled;
    const slot = this.currentSlot(reservation);
    if (slot?.state !== "retained" || !slot.effect) return { kind: "none" };
    if (slot.effect.eventJson !== replayEventJson) return { kind: "mismatch" };
    slot.state = "claimed";
    return { kind: "claimed", effect: slot.effect };
  }

  complete(
    reservation: PendingEventPublicationReservation,
    effect: CommittedEventPublication,
  ): void {
    const slot = this.currentSlot(reservation);
    if (slot?.state === "claimed" && slot.effect === effect) {
      this.removeCurrent(reservation);
    }
  }

  restore(
    reservation: PendingEventPublicationReservation,
    effect: CommittedEventPublication,
  ): void {
    const slot = this.currentSlot(reservation);
    if (slot?.state === "claimed" && slot.effect === effect) {
      slot.state = "retained";
    }
  }

  clearWorker(workerFp: string): number {
    const slots = this.byWorker.get(workerFp);
    if (!slots) return 0;
    this.byWorker.delete(workerFp);
    this.retainedEntries -= slots.size;
    for (const slot of slots.values()) slot.settle();
    return slots.size;
  }

  private currentSlot(
    reservation: PendingEventPublicationReservation,
  ): PendingPublicationSlot | undefined {
    const slot = this.byWorker
      .get(reservation.workerFp)
      ?.get(reservation.clientSeq);
    return slot?.token === reservation.token ? slot : undefined;
  }

  private removeCurrent(
    reservation: PendingEventPublicationReservation,
  ): void {
    const workerSlots = this.byWorker.get(reservation.workerFp);
    const slot = workerSlots?.get(reservation.clientSeq);
    if (!workerSlots || slot?.token !== reservation.token) return;
    workerSlots.delete(reservation.clientSeq);
    if (workerSlots.size === 0) this.byWorker.delete(reservation.workerFp);
    this.retainedEntries -= 1;
    slot.settle();
  }
}

export async function reservePendingEventPublication(
  store: PendingEventPublicationStore | undefined,
  workerFp: string | null,
  clientSeq: number | null,
): Promise<PendingEventPublicationReservation | undefined> {
  if (!store || workerFp === null || clientSeq === null) return undefined;
  let reservation = store.reserve(workerFp, clientSeq);
  if (!reservation.ownsReservation) {
    await reservation.settled;
    reservation = store.reserve(workerFp, clientSeq);
  }
  return reservation;
}

export async function resolveEventPublication(
  options: ResolveEventPublicationOptions,
): Promise<EventPublicationResolution> {
  const {
    store,
    reservation,
    committedEffect,
    deduplicated,
    canPublish,
    replayEventJson,
  } = options;
  if (committedEffect) {
    if (!(canPublish?.() ?? true)) {
      if (store && reservation) store.retain(reservation, committedEffect);
      return { replayRejected: false };
    }
    try {
      publishCommittedEvent(committedEffect);
    } catch (error) {
      if (store && reservation) store.retain(reservation, committedEffect);
      throw error;
    }
    if (store && reservation) store.release(reservation);
    return { publishedEffect: committedEffect, replayRejected: false };
  }

  if (!deduplicated || !store || !reservation) {
    if (store && reservation) store.release(reservation);
    return { replayRejected: false };
  }
  if (!(canPublish?.() ?? true)) {
    store.release(reservation);
    return { replayRejected: false };
  }
  const claim = await store.claim(reservation, replayEventJson);
  if (claim.kind !== "claimed") {
    if (claim.kind === "mismatch") return { replayRejected: true };
    store.release(reservation);
    return { replayRejected: false };
  }
  if (!(canPublish?.() ?? true)) {
    store.restore(reservation, claim.effect);
    return { replayRejected: false };
  }
  try {
    publishCommittedEvent(claim.effect);
  } catch (error) {
    store.restore(reservation, claim.effect);
    throw error;
  }
  store.complete(reservation, claim.effect);
  return { publishedEffect: claim.effect, replayRejected: false };
}

function publishCommittedEvent(effect: CommittedEventPublication): void {
  applyDurableChannelIndex(effect.event, effect.authenticatedWorkerFp);
  const stamped: SessionBusMessage = {
    ...effect.event,
    _dashboard_id: effect.dashboardId,
    _event_id: effect.eventId,
  };
  sessionBus.publish(stamped);
  for (const id of effect.cascadeOrphanIds) {
    workspaceBus.publish({
      kind: "deleted",
      id: asWorkspaceId(id),
      _dashboard_id: effect.dashboardId,
    } as Parameters<typeof workspaceBus.publish>[0]);
  }
}

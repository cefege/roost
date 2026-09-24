// A staged direct candidate owns unrendered frame continuity and prospective views.
// It has no route-election authority; the direct registry invokes its prepared commit seam.
// Candidate bytes remain document-budgeted until commit or cancellation releases them.
import { create } from "@bufbuild/protobuf";
import {
  CellGridChunkAssembler, encodedCellGridChunkSize, encodedCellGridFrameSize, type CellGridFrame,
} from "@roost/protocol/cell";
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/protocol/proto/cell_pb";
import {
  TerminalViewCommandSchema, TerminalViewStatus, type TerminalViewStateFrame,
} from "@roost/protocol/proto/sync_pb";
import { isTerminalGeometry, isTerminalUuid } from "@roost/protocol/viewport";
import { pushTerminalChunkTransfer, resetTerminalChunkTransfer, type TerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import { decodeTerminalWireFrame, foldTerminalFrame, type TerminalFrameFoldTarget } from "../client/terminal-stream/terminal-stream-frame-fold.ts";
import { terminalGenerationMatches } from "./terminal-stream-liveness.ts";
import { terminalTransportTargetForToken } from "./terminal-stream-publication.ts";
import { applyTerminalPromotionCanonical } from "./terminal-stream-promotion-commit.ts";
import { armTerminalViewRenewal, cancelTerminalViewRenewal, type TerminalViewRenewalTarget } from "./terminal-stream-renewal-scheduler.ts";
import { noteTerminalCellFrame } from "./terminal-stream-repair.ts";
import { deliverTerminalCanonicalFull, notifyTerminalBaselineState } from "./terminal-stream-replica.ts";
import type { TerminalGenerationToken, TerminalSessionReplica, TerminalViewIntent, TerminalViewRecord } from "./terminal-stream-types.ts";
import type { TerminalDirectConnection, TerminalDirectPromotionPrepared } from "./terminal-stream-transport.ts";
import { notifyTerminalBaselineProgress } from "./terminal-stream-progress.ts";
const TERMINAL_PROMOTION_BASELINE_MS = 5_000;
export interface TerminalPromotionSourceBudget {
  reserve(bytes: number): boolean;
  release(bytes: number): void;
}
export interface TerminalSessionPromotion {
  readonly sessionId: string;
  readonly token: TerminalGenerationToken;
  readonly attemptId: string;
  readonly prospectiveViewIds: readonly string[];
  isReady(): boolean;
  connection(): TerminalDirectConnection;
  awaitReady(): Promise<boolean>;
  prepare(claimEpoch: string, oldToken: TerminalGenerationToken | null): TerminalDirectPromotionPrepared | null;
  cancel(reason: string): void;
}
export interface TerminalSessionPromotionOptions {
  readonly sessionId: string;
  readonly attemptId: string;
  readonly connection: TerminalDirectConnection;
  readonly token: TerminalGenerationToken;
  readonly onCancelled?: (reason: string) => void;
}

export interface TerminalPromotionCandidateView extends TerminalViewRenewalTarget {
  readonly source: TerminalViewRecord;
  readonly oldViewId: string;
  readonly viewId: string;
  readonly intent: TerminalViewIntent;
  readonly sourceIntent: TerminalViewIntent;
  acknowledged: boolean;
  disposed: boolean;
}

export interface TerminalPromotionCandidateRegistry {
  unregister(candidate: TerminalPromotionCandidate): void;
}

export class TerminalPromotionCandidate implements TerminalSessionPromotion, TerminalFrameFoldTarget, TerminalChunkTransfer {
  readonly sessionId: string;
  readonly token: TerminalGenerationToken;
  readonly attemptId: string;
  readonly prospectiveViewIds: readonly string[];
  readonly assembler = new CellGridChunkAssembler();
  readonly views = new Map<string, TerminalPromotionCandidateView>();
  expectedStreamId: string | null = null;
  effectiveCols = 0;
  effectiveRows = 0;
  canonical: CellGridFrame | null = null;
  baselineReady = false;
  chunkTimer: Timer | null = null;
  #baselineTimer: Timer | null = null;
  #baselineSourceBytes = 0;
  #deltaSourceBytes = 0;
  #assemblySourceBytes = 0;
  #disposed = false;
  #ready = Promise.withResolvers<boolean>();

  constructor(
    readonly session: TerminalSessionReplica,
    private readonly options: TerminalSessionPromotionOptions,
    private readonly sourceBudget: TerminalPromotionSourceBudget,
    private readonly registry: TerminalPromotionCandidateRegistry,
  ) {
    this.sessionId = options.sessionId;
    this.token = options.token;
    this.attemptId = options.attemptId;
    for (const source of session.handles.values()) {
      const desired = source.desired;
      if (source.disposed || !desired?.active) continue;
      const viewId = crypto.randomUUID();
      const sourceIntent = { ...desired };
      const intent = { ...sourceIntent, revision: sourceIntent.revision + 1n };
      const candidateView: TerminalPromotionCandidateView = {
        source,
        sourceIntent,
        oldViewId: source.viewId,
        viewId,
        intent,
        desired: intent,
        acknowledged: false,
        disposed: false,
        renewalDueAtMs: null,
        renew: () => this.publishView(candidateView, true),
      };
      this.views.set(viewId, candidateView);
    }
    this.prospectiveViewIds = [...this.views.keys()];
  }

  start(): boolean {
    if (this.views.size === 0) return false;
    this.#baselineTimer = setTimeout(
      () => this.cancel("candidate baseline timed out"),
      TERMINAL_PROMOTION_BASELINE_MS,
    );
    for (const view of this.views.values()) {
      this.publishView(view, true);
      armTerminalViewRenewal(view);
    }
    return !this.#disposed;
  }

  isReady(): boolean {
    return !this.#disposed && this.baselineReady && this.canonical !== null;
  }
  awaitReady(): Promise<boolean> {
    return this.#ready.promise;
  }

  connection(): TerminalDirectConnection { return this.options.connection; }
  acceptViewState(frame: TerminalViewStateFrame): void {
    const view = this.views.get(frame.viewId);
    if (
      !view
      || frame.sessionId !== this.sessionId
      || frame.revision !== view.intent.revision
      || frame.status !== TerminalViewStatus.ACCEPTED
      || !frame.active
      || !isTerminalUuid(frame.streamId)
      || !isTerminalGeometry({ cols: frame.effectiveCols, rows: frame.effectiveRows })
    ) {
      this.cancel("candidate view state rejected");
      return;
    }
    if (this.expectedStreamId !== null && (
      this.expectedStreamId !== frame.streamId
      || this.effectiveCols !== frame.effectiveCols
      || this.effectiveRows !== frame.effectiveRows
    )) {
      this.cancel("candidate views disagreed about stream state");
      return;
    }
    this.expectedStreamId = frame.streamId;
    this.effectiveCols = frame.effectiveCols;
    this.effectiveRows = frame.effectiveRows;
    view.acknowledged = true;
  }

  acceptCellFrame(frame: PbCellGridFrame, assembled: boolean): void {
    if (!this.acceptsFrame(frame)) return;
    if (frame.full) this.releaseFoldedSource();
    const sourceBytes = encodedCellGridFrameSize(frame);
    if (!this.sourceBudget.reserve(sourceBytes)) {
      this.cancel("candidate source budget exceeded");
      return;
    }
    const decoded = decodeTerminalWireFrame(frame, assembled);
    if (decoded.kind === "invalid") {
      this.sourceBudget.release(sourceBytes);
      this.cancel("candidate frame was malformed");
      return;
    }
    const result = foldTerminalFrame(this, decoded.frame);
    if (result.kind === "invalid") {
      this.sourceBudget.release(sourceBytes);
      this.cancel("candidate frame did not continue its baseline");
      return;
    }
    if (result.kind === "full") {
      this.releaseAssemblySource();
      resetTerminalChunkTransfer(this);
      this.#baselineSourceBytes = sourceBytes;
      this.clearBaselineDeadline();
      this.#ready.resolve(true);
    } else {
      this.#deltaSourceBytes += sourceBytes;
    }
  }

  acceptCellChunk(chunk: PbCellGridChunk): void {
    const part = chunk.part;
    if (!part || !this.acceptsFrame(part)) return;
    if (chunk.chunkIndex === 0 && this.assembler.activeSnapshotId !== chunk.snapshotId) {
      this.releaseAssemblySource();
    }
    const sourceBytes = encodedCellGridChunkSize(chunk);
    if (!this.sourceBudget.reserve(sourceBytes)) {
      this.cancel("candidate source budget exceeded");
      return;
    }
    this.#assemblySourceBytes += sourceBytes;
    pushTerminalChunkTransfer(this, chunk, {
      onComplete: (frame) => this.acceptAssembledFrame(frame),
      onInvalid: () => {
        this.releaseAssemblySource();
        this.cancel("candidate chunk transfer was malformed");
      },
      onProgress: () => {},
      onChange: () => {},
    });
  }

  prepare(claimEpoch: string, oldToken: TerminalGenerationToken | null): TerminalDirectPromotionPrepared | null {
    if (!this.isReady() || !this.allViewsAcknowledged() || !this.expectedStreamId || !this.canonical) return null;
    const prospectiveViews = new Map(
      [...this.views.values()].map((view) => [view.oldViewId, {
        viewId: view.viewId,
        intent: view.intent,
        acknowledged: view.acknowledged,
      }]),
    );
    return {
      attemptId: this.attemptId,
      connection: this.options.connection,
      token: this.token,
      oldToken,
      currentToken: this.session.generation,
      claimEpoch,
      candidateFrame: this.canonical,
      expectedStreamId: this.expectedStreamId,
      prospectiveViews,
      applyCanonical: () => this.applyCanonical(oldToken),
    };
  }

  cancel(reason: string): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.clearBaselineDeadline();
    this.#ready.resolve(false);
    resetTerminalChunkTransfer(this);
    this.releaseAllSource();
    for (const view of this.views.values()) {
      cancelTerminalViewRenewal(view);
      view.disposed = true;
      this.publishView(view, false);
    }
    this.registry.unregister(this);
    this.options.onCancelled?.(reason);
  }

  private acceptsFrame(frame: Pick<PbCellGridFrame, "sessionId" | "streamId">): boolean {
    if (
      this.#disposed
      || !this.allViewsAcknowledged()
      || frame.sessionId !== this.sessionId
      || frame.streamId !== this.expectedStreamId
    ) {
      this.cancel("candidate received a stale frame");
      return false;
    }
    return true;
  }

  private acceptAssembledFrame(frame: PbCellGridFrame): void {
    const sourceBytes = this.#assemblySourceBytes;
    const decoded = decodeTerminalWireFrame(frame, true);
    if (decoded.kind === "invalid") {
      this.releaseAssemblySource();
      this.cancel("candidate chunk frame was malformed");
      return;
    }
    const result = foldTerminalFrame(this, decoded.frame);
    if (result.kind !== "full") {
      this.releaseAssemblySource();
      this.cancel("candidate chunk frame did not establish a baseline");
      return;
    }
    this.releaseFoldedSource();
    this.#baselineSourceBytes = sourceBytes;
    this.#assemblySourceBytes = 0;
    this.clearBaselineDeadline();
    this.#ready.resolve(true);
  }

  private applyCanonical(oldToken: TerminalGenerationToken | null): boolean {
    if (!this.canCommit(oldToken) || !this.canonical || !this.expectedStreamId) return false;
    const result = applyTerminalPromotionCanonical({
      session: this.session,
      token: this.token,
      expectedStreamId: this.expectedStreamId,
      effectiveCols: this.effectiveCols,
      effectiveRows: this.effectiveRows,
      canonical: this.canonical,
      views: [...this.views.values()],
    });
    noteTerminalCellFrame(this.session, this.canonical, true, this.token);
    this.#disposed = true;
    this.clearBaselineDeadline();
    resetTerminalChunkTransfer(this);
    this.releaseAllSource();
    this.registry.unregister(this);
    queueMicrotask(() => {
      if (!terminalGenerationMatches(this.session.generation, this.token)) return;
      notifyTerminalBaselineProgress(this.session);
      deliverTerminalCanonicalFull(this.session);
      notifyTerminalBaselineState(this.session);
      this.retirePreviousViews(result.previousViews, oldToken);
    });
    return true;
  }

  private canCommit(oldToken: TerminalGenerationToken | null): boolean {
    if (this.#disposed || !this.allViewsAcknowledged() || !this.canonical) return false;
    if (oldToken === null ? this.session.generation !== null : !terminalGenerationMatches(this.session.generation, oldToken)) return false;
    for (const view of this.views.values()) {
      const sourceIntent = view.source.desired;
      if (
        view.source.disposed
        || view.source.viewId !== view.oldViewId
        || !sourceIntent
        || sourceIntent.revision !== view.sourceIntent.revision
        || !sourceIntent.active
        || sourceIntent.cols !== view.sourceIntent.cols
        || sourceIntent.rows !== view.sourceIntent.rows
      ) return false;
    }
    const current = this.session.canonical;
    return current === null
      || current.streamId !== this.canonical.streamId
      || current.gridEpoch !== this.canonical.gridEpoch
      || this.canonical.seq >= current.seq;

  }
  private publishView(view: TerminalPromotionCandidateView, active: boolean): void {
    if (this.#disposed && active) return;
    try {
      this.options.connection.publishView(create(TerminalViewCommandSchema, {
        viewId: view.viewId,
        sessionId: this.sessionId,
        cols: active ? view.intent.cols : 0,
        rows: active ? view.intent.rows : 0,
        revision: active ? view.intent.revision : view.intent.revision + 1n,
        active,
        domainGeneration: this.token.domainGeneration,
      }));
    } catch {
      if (active) this.cancel("candidate view publication failed");
    }
  }

  private allViewsAcknowledged(): boolean {
    return this.views.size > 0 && [...this.views.values()].every((view) => view.acknowledged);
  }

  private releaseFoldedSource(): void {
    this.sourceBudget.release(this.#baselineSourceBytes + this.#deltaSourceBytes);
    this.#baselineSourceBytes = 0;
    this.#deltaSourceBytes = 0;
  }

  private releaseAssemblySource(): void {
    this.sourceBudget.release(this.#assemblySourceBytes);
    this.#assemblySourceBytes = 0;
  }

  private releaseAllSource(): void {
    this.releaseFoldedSource();
    this.releaseAssemblySource();
  }

  private clearBaselineDeadline(): void {
    clearTimeout(this.#baselineTimer ?? undefined);
    this.#baselineTimer = null;
  }

  private retirePreviousViews(
    views: readonly TerminalPromotionCandidateView[],
    oldToken: TerminalGenerationToken | null,
  ): void {
    const target = oldToken ? terminalTransportTargetForToken(oldToken) : null;
    if (!target) return;
    for (const view of views) {
      target.publishView(create(TerminalViewCommandSchema, {
        viewId: view.oldViewId,
        sessionId: this.sessionId,
        cols: 0,
        rows: 0,
        revision: view.intent.revision,
        active: false,
        domainGeneration: target.domainGeneration,
      }));
    }
  }
}

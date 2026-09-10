// Retains one compact terminal-metadata frame per worker channel outside the
// ordinary announced-cell buffer. AnnouncedChannelBarrier moves an early record
// into ordered delivery or parks it after cell loss until the exact route binds.
// This owner accepts no PTY bytes and shares the worker socket work budget.
import { create, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  WTerminalMetadataSchema,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import type { WorkerRetainedWorkBudget } from "./worker-frame-queue.ts";

export const ANNOUNCED_SEMANTIC_METADATA_MAX_CHANNELS = 64;
export const ANNOUNCED_SEMANTIC_METADATA_MAX_BYTES = 4 * 1024;
export const ANNOUNCED_SEMANTIC_METADATA_PREANNOUNCE_MAX_MS = 3_000;
export const ANNOUNCED_SEMANTIC_METADATA_RECOVERY_MAX_MS = 30_000;

type Timer = NodeJS.Timeout;

export interface RetainedTerminalMetadataFrame {
  frame: CoordWorkerUp;
  encodedBytes: number;
  retained: boolean;
}

interface TimedTerminalMetadata extends RetainedTerminalMetadataFrame {
  sessionId: string | null;
  timer: Timer;
}

export interface MergedTerminalMetadataFrame {
  frame: CoordWorkerUp;
  encodedBytes: number;
}

export interface AnnouncedSemanticMetadataStats {
  frames: number;
  bytes: number;
  preAnnounced: number;
  recovery: number;
}

export function isCompactTerminalMetadataFrame(
  frame: CoordWorkerUp,
  encodedBytes: number,
): boolean {
  return frame.frame.case === "terminalMetadata"
    && Number.isSafeInteger(encodedBytes)
    && encodedBytes > 0
    && encodedBytes <= ANNOUNCED_SEMANTIC_METADATA_MAX_BYTES;
}

export function mergeTerminalMetadataFrames(
  previous: CoordWorkerUp,
  incoming: CoordWorkerUp,
): MergedTerminalMetadataFrame {
  if (
    previous.frame.case !== "terminalMetadata"
    || incoming.frame.case !== "terminalMetadata"
  ) {
    throw new Error("only terminal metadata may be coalesced");
  }
  const prior = previous.frame.value;
  const next = incoming.frame.value;
  const frame = create(CoordWorkerUpSchema, {
    frame: {
      case: "terminalMetadata",
      value: create(WTerminalMetadataSchema, {
        channelId: next.channelId,
        titleChanged: prior.titleChanged || next.titleChanged,
        title: next.titleChanged ? next.title : prior.title,
        activityChanged: prior.activityChanged || next.activityChanged,
        activityTsMs: next.activityChanged ? next.activityTsMs : prior.activityTsMs,
      }),
    },
  });
  return { frame, encodedBytes: toBinary(CoordWorkerUpSchema, frame).byteLength };
}

export class AnnouncedChannelSemanticRetention {
  private readonly preAnnounced = new Map<number, TimedTerminalMetadata>();
  private readonly recovery = new Map<number, TimedTerminalMetadata>();
  private readonly drainingRecoveryChannels = new Set<number>();
  private retainedWorkBudget: WorkerRetainedWorkBudget | null;

  constructor(retainedWorkBudget?: WorkerRetainedWorkBudget) {
    this.retainedWorkBudget = retainedWorkBudget ?? null;
  }

  bindRetainedWorkBudget(retainedWorkBudget: WorkerRetainedWorkBudget): void {
    if (this.retainedWorkBudget === retainedWorkBudget) return;
    if (this.retainedWorkBudget || this.recordCount() !== 0) {
      throw new Error("announced semantic metadata budget already bound");
    }
    this.retainedWorkBudget = retainedWorkBudget;
  }
  retainUnannounced(
    channelId: number,
    frame: CoordWorkerUp,
    encodedBytes: number,
    routeSessionId: string | null = null,
  ): boolean {
    if (this.drainingRecoveryChannels.has(channelId)) {
      return this.replace(
        this.preAnnounced,
        channelId,
        routeSessionId,
        frame,
        encodedBytes,
        ANNOUNCED_SEMANTIC_METADATA_RECOVERY_MAX_MS,
      );
    }
    const recovery = this.recovery.get(channelId);
    if (recovery) {
      return this.replace(
        this.recovery,
        channelId,
        recovery.sessionId,
        frame,
        encodedBytes,
        ANNOUNCED_SEMANTIC_METADATA_RECOVERY_MAX_MS,
      );
    }
    return this.replace(
      this.preAnnounced,
      channelId,
      null,
      frame,
      encodedBytes,
      ANNOUNCED_SEMANTIC_METADATA_PREANNOUNCE_MAX_MS,
    );
  }

  takePreAnnounced(channelId: number): RetainedTerminalMetadataFrame | undefined {
    return this.take(this.preAnnounced, channelId);
  }

  parkRecovery(
    channelId: number,
    sessionId: string,
    frame: RetainedTerminalMetadataFrame,
  ): boolean {
    this.discard(this.preAnnounced, channelId);
    const previous = this.recovery.get(channelId);
    if (!previous && this.recordCount() >= ANNOUNCED_SEMANTIC_METADATA_MAX_CHANNELS) {
      this.release(frame);
      return false;
    }
    let recoveryFrame = frame;
    if (previous?.sessionId === sessionId) {
      const merged = mergeTerminalMetadataFrames(previous.frame, frame.frame);
      this.discard(this.recovery, channelId);
      this.release(frame);
      if (!isCompactTerminalMetadataFrame(merged.frame, merged.encodedBytes)) return false;
      if (this.requireBudget().retain(merged.encodedBytes) !== "retained") return false;
      recoveryFrame = { ...merged, retained: true };
    } else if (previous) {
      this.discard(this.recovery, channelId);
    }
    this.install(
      this.recovery,
      channelId,
      sessionId,
      recoveryFrame,
      ANNOUNCED_SEMANTIC_METADATA_RECOVERY_MAX_MS,
    );
    return true;
  }

  takeRecoveryForSession(
    channelId: number,
    sessionId: string,
  ): RetainedTerminalMetadataFrame | undefined {
    const record = this.recoveryForSession(channelId, sessionId);
    if (!record || this.drainingRecoveryChannels.has(channelId)) return undefined;
    return this.take(this.recovery, channelId);
  }
  // A durable route replacement must not let its predecessor consume a new fact.
  reconcileMappedRoute(channelId: number, sessionId: string): boolean {
    const recovery = this.recovery.get(channelId);
    if (recovery) {
      if (recovery.sessionId === sessionId) return true;
      this.discard(this.recovery, channelId);
    }
    this.discard(this.preAnnounced, channelId);
    return false;
  }

  discardRecovery(channelId: number): void {
    this.discard(this.recovery, channelId);
  }
  async commitRecovery(
    channelId: number,
    sessionId: string,
    mappingMatches: () => boolean,
    deliver: (frame: CoordWorkerUp) => Promise<void>,
  ): Promise<boolean> {
    let record = this.recovery.get(channelId);
    if (!record || record.sessionId !== sessionId) return false;
    // Keep the record routable while await yields so later facts queue behind it.
    this.drainingRecoveryChannels.add(channelId);
    try {
      while (record) {
        if (!mappingMatches()) {
          if (this.recovery.get(channelId) === record) this.discard(this.recovery, channelId);
          if (this.preAnnounced.get(channelId)?.sessionId === sessionId) {
            this.discard(this.preAnnounced, channelId);
          }
          return false;
        }
        clearTimeout(record.timer);
        await deliver(record.frame);
        if (this.recovery.get(channelId) !== record) {
          const replacement = this.recovery.get(channelId);
          if (!replacement || replacement.sessionId !== sessionId) return true;
          record = replacement;
          continue;
        }
        if (!mappingMatches()) {
          if (this.recovery.get(channelId) === record) this.discard(this.recovery, channelId);
          if (this.preAnnounced.get(channelId)?.sessionId === sessionId) {
            this.discard(this.preAnnounced, channelId);
          }
          return false;
        }
        const pending = this.take(this.preAnnounced, channelId);
        this.discard(this.recovery, channelId);
        if (pending && pending.sessionId !== sessionId) {
          this.release(pending);
          return false;
        }
        if (!pending) return true;
        this.install(
          this.recovery,
          channelId,
          sessionId,
          pending,
          ANNOUNCED_SEMANTIC_METADATA_RECOVERY_MAX_MS,
        );
        record = this.recovery.get(channelId);
      }
      return true;
    } catch (error) {
      if (this.recovery.get(channelId) === record) this.discard(this.recovery, channelId);
      if (this.preAnnounced.get(channelId)?.sessionId === sessionId) {
        this.discard(this.preAnnounced, channelId);
      }
      throw error;
    } finally {
      this.drainingRecoveryChannels.delete(channelId);
    }
  }


  clear(): void {
    this.clearRecords(this.preAnnounced);
    this.clearRecords(this.recovery);
    this.drainingRecoveryChannels.clear();
  }

  stats(): AnnouncedSemanticMetadataStats {
    let bytes = 0;
    for (const record of this.preAnnounced.values()) bytes += record.encodedBytes;
    for (const record of this.recovery.values()) bytes += record.encodedBytes;
    return {
      frames: this.recordCount(),
      bytes,
      preAnnounced: this.preAnnounced.size,
      recovery: this.recovery.size,
    };
  }

  private replace(
    records: Map<number, TimedTerminalMetadata>,
    channelId: number,
    sessionId: string | null,
    frame: CoordWorkerUp,
    encodedBytes: number,
    timeoutMs: number,
  ): boolean {
    if (!isCompactTerminalMetadataFrame(frame, encodedBytes)) return false;
    const previous = records.get(channelId);
    const compact = previous
      ? mergeTerminalMetadataFrames(previous.frame, frame)
      : { frame, encodedBytes };
    if (!isCompactTerminalMetadataFrame(compact.frame, compact.encodedBytes)) return false;
    if (
      !previous
      && this.recordCount() >= ANNOUNCED_SEMANTIC_METADATA_MAX_CHANNELS
      && !(records === this.preAnnounced && this.drainingRecoveryChannels.has(channelId))
    ) return false;
    if (previous) this.discard(records, channelId);
    const retainedWorkBudget = this.requireBudget();
    if (retainedWorkBudget.retain(compact.encodedBytes) !== "retained") return false;
    this.install(
      records,
      channelId,
      sessionId,
      { ...compact, retained: true },
      timeoutMs,
    );
    return true;
  }

  private install(
    records: Map<number, TimedTerminalMetadata>,
    channelId: number,
    sessionId: string | null,
    frame: RetainedTerminalMetadataFrame,
    timeoutMs: number,
  ): void {
    const record: TimedTerminalMetadata = {
      ...frame,
      sessionId,
      timer: undefined as unknown as Timer,
    };
    record.timer = setTimeout(() => {
      if (records.get(channelId) !== record) return;
      this.discard(records, channelId);
    }, timeoutMs);
    record.timer.unref?.();
    records.set(channelId, record);
  }

  private take(
    records: Map<number, TimedTerminalMetadata>,
    channelId: number,
  ): TimedTerminalMetadata | undefined {
    const record = records.get(channelId);
    if (!record) return undefined;
    clearTimeout(record.timer);
    records.delete(channelId);
    return record;
  }

  private recoveryForSession(
    channelId: number,
    sessionId: string,
  ): TimedTerminalMetadata | undefined {
    const record = this.recovery.get(channelId);
    if (!record || record.sessionId === sessionId) return record;
    this.discard(this.recovery, channelId);
    return undefined;
  }
  private discard(records: Map<number, TimedTerminalMetadata>, channelId: number): void {
    const record = records.get(channelId);
    if (!record) return;
    clearTimeout(record.timer);
    records.delete(channelId);
    this.release(record);
  }

  private clearRecords(records: Map<number, TimedTerminalMetadata>): void {
    for (const record of records.values()) {
      clearTimeout(record.timer);
      this.release(record);
    }
    records.clear();
  }

  private release(frame: RetainedTerminalMetadataFrame): void {
    if (!frame.retained) return;
    frame.retained = false;
    this.requireBudget().release(frame.encodedBytes);
  }

  private recordCount(): number {
    return this.preAnnounced.size + this.recovery.size;
  }

  private requireBudget(): WorkerRetainedWorkBudget {
    if (!this.retainedWorkBudget) {
      throw new Error("announced semantic metadata budget is not bound");
    }
    return this.retainedWorkBudget;
  }
}

// The promotion commit is the one synchronous canonical mutation boundary.
// It transfers staged view leases only after callers fenced route and attempt identity.
// Renderer/status notifications are deliberately returned to the candidate for deferred delivery.
// This module neither selects routes nor receives direct network frames.

import { TERMINAL_VIEW_LEASE_MS } from "@roost/protocol/viewport";
import { resetTerminalChunkTransfer } from "./terminal-stream-chunks.ts";
import { clearTerminalSessionLiveness } from "./terminal-stream-liveness.ts";
import {
  beginTerminalViewRenewalBatch,
  endTerminalViewRenewalBatch,
  transferTerminalViewRenewal,
} from "./terminal-stream-renewal-scheduler.ts";
import { clearViewAck } from "./terminal-stream-view-commands.ts";
import type { TerminalPromotionCandidateView } from "./terminal-stream-promotion-candidate.ts";
import type {
  TerminalGenerationToken,
  TerminalSessionReplica,
  TerminalViewHandleStatus,
  TerminalViewRecord,
} from "./terminal-stream-types.ts";

export interface TerminalPromotionCommitResult {
  readonly statuses: ReadonlyArray<{
    readonly view: TerminalViewRecord;
    readonly status: TerminalViewHandleStatus;
  }>;
  readonly previousViews: readonly TerminalPromotionCandidateView[];
}

export function applyTerminalPromotionCanonical(input: {
  session: TerminalSessionReplica;
  token: TerminalGenerationToken;
  expectedStreamId: string;
  effectiveCols: number;
  effectiveRows: number;
  canonical: NonNullable<TerminalSessionReplica["canonical"]>;
  views: readonly TerminalPromotionCandidateView[];
}): TerminalPromotionCommitResult {
  const statuses: Array<{
    view: TerminalViewRecord;
    status: TerminalViewHandleStatus;
  }> = [];
  beginTerminalViewRenewalBatch();
  try {
    resetTerminalChunkTransfer(input.session);
    clearTerminalSessionLiveness(input.session, "generation_reset");
    input.session.generation = input.token;
    input.session.expectedStreamId = input.expectedStreamId;
    input.session.effectiveCols = input.effectiveCols;
    input.session.effectiveRows = input.effectiveRows;
    input.session.canonical = input.canonical;
    input.session.baselineReady = true;
    input.session.requiresFreshBaseline = false;
    const leaseDeadlineMs = Date.now() + TERMINAL_VIEW_LEASE_MS;
    for (const view of input.views) {
      const source = view.source;
      input.session.handles.delete(view.oldViewId);
      source.viewId = view.viewId;
      source.revisionFloor = view.intent.revision;
      source.desired = { ...view.intent };
      source.accepted = { ...view.intent };
      source.rollingBack = false;
      source.leaseDeadlineMs = leaseDeadlineMs;
      clearViewAck(source);
      const status: TerminalViewHandleStatus = {
        status: "accepted",
        revision: view.intent.revision,
        active: true,
        streamId: input.expectedStreamId,
        effectiveCols: input.effectiveCols,
        effectiveRows: input.effectiveRows,
        baselineReady: true,
      };
      source.status = status;
      statuses.push({ view: source, status });
      input.session.handles.set(view.viewId, source);
      transferTerminalViewRenewal(view, source);
      view.disposed = true;
    }
  } finally {
    endTerminalViewRenewalBatch();
  }
  return { statuses, previousViews: input.views };
}

// Canonical Zod ↔ protobuf adapter for terminal sessions.

import { create } from "@bufbuild/protobuf";
import {
  SessionSchema,
  type Session as PbSession,
} from "../gen/roost/v1/wire_pb.ts";
import { Session as SessionZ, type Session } from "./session.ts";

export function sessionToProto(s: Session): PbSession {
  return create(SessionSchema, {
    id: s.id,
    workerFp: s.worker_fp,
    channel: s.channel,
    kind: s.kind,
    cwd: s.cwd,
    workspaceId: s.workspace_id ?? undefined,
    status: s.status,
    createdAt: BigInt(s.created_at),
    closedAt: s.closed_at !== null ? BigInt(s.closed_at) : undefined,
    customTitle: s.custom_title ?? undefined,
    gitBranch: s.git_branch ?? undefined,
    gitRemote: s.git_remote ?? undefined,
    prNumber: s.pr_number ?? undefined,
    prState: s.pr_state ?? undefined,
    prChecks: s.pr_checks ?? undefined,
    prUrl: s.pr_url ?? undefined,
    ports: s.ports ?? [],
    spawnCwd: s.spawn_cwd ?? undefined,
  });
}

// Re-parse at the protobuf boundary so brands and enum membership are enforced
// before a session reaches the projector.
export function sessionFromProto(p: PbSession): Session {
  return SessionZ.parse({
    id: p.id,
    worker_fp: p.workerFp,
    channel: p.channel,
    kind: p.kind,
    cwd: p.cwd,
    workspace_id: p.workspaceId ? p.workspaceId : null,
    status: p.status,
    created_at: Number(p.createdAt),
    closed_at: p.closedAt !== undefined ? Number(p.closedAt) : null,
    custom_title: p.customTitle ?? null,
    ...(p.gitBranch !== undefined ? { git_branch: p.gitBranch } : {}),
    ...(p.gitRemote !== undefined ? { git_remote: p.gitRemote } : {}),
    ...(p.prNumber !== undefined ? { pr_number: p.prNumber } : {}),
    ...(p.prState !== undefined ? { pr_state: p.prState } : {}),
    ...(p.prChecks !== undefined ? { pr_checks: p.prChecks } : {}),
    ...(p.prUrl !== undefined ? { pr_url: p.prUrl } : {}),
    ...(p.ports.length > 0 ? { ports: p.ports } : {}),
    ...(p.spawnCwd !== undefined ? { spawn_cwd: p.spawnCwd } : {}),
  });
}

// Canonical Zod ↔ proto adapters for structured AgentState + Session.
// Single source of truth — every caller composes from here.
//
// Callers:
//   - apps/shared/src/wire/event-proto.ts (agentPatchToProto / snapshot Session[])
//   - apps/coord/src/connect/router.ts (sessionRowToProto)
//
// Drift caught here = drift fixed everywhere. Adding an AgentState field
// requires editing only this module.

import { create } from "@bufbuild/protobuf";
import {
  TokensSchema, LastMessageSchema, CurrentToolSchema,
  CurrentBlockSchema, PermissionRequestSchema, SubAgentRowSchema,
  AgentStateSchema, SessionSchema,
  AgentEntrySchema, AgentTextEntrySchema, AgentToolEntrySchema,
  AgentPromptEntrySchema, AgentNoticeEntrySchema, AgentTodoEntrySchema,
  AgentSubagentEntrySchema, AgentImageEntrySchema,
  type Tokens as PbTokens,
  type LastMessage as PbLastMessage,
  type CurrentTool as PbCurrentTool,
  type CurrentBlock as PbCurrentBlock,
  type PermissionRequest as PbPermissionRequest,
  type SubAgentRow as PbSubAgentRow,
  type AgentState as PbAgentState,
  type Session as PbSession,
  type AgentEntry as PbAgentEntry,
} from "../gen/roost/v1/wire_pb.ts";
import {
  AgentState as AgentStateZ, Session as SessionZ,
  type Tokens, type LastMessage, type CurrentTool, type CurrentBlock,
  type PermissionRequest, type SubAgentRow, type AgentState,
  type Session,
} from "./session.ts";
import { AgentEntry as AgentEntryZ, type AgentEntry } from "./agent-entry.ts";

// ─── Tokens ────────────────────────────────────────────────────────────────
export function tokensToProto(t: Tokens): PbTokens {
  return create(TokensSchema, {
    in: BigInt(t.in), out: BigInt(t.out), cached: BigInt(t.cached),
  });
}
export function tokensFromProto(p: PbTokens): Tokens {
  return { in: Number(p.in), out: Number(p.out), cached: Number(p.cached) };
}

// ─── LastMessage ───────────────────────────────────────────────────────────
export function lastMessageToProto(m: LastMessage): PbLastMessage {
  return create(LastMessageSchema, { role: m.role, text: m.text, ts: BigInt(m.ts) });
}
export function lastMessageFromProto(p: PbLastMessage): LastMessage {
  return {
    role: p.role as LastMessage["role"],
    text: p.text,
    ts: Number(p.ts),
  };
}

// ─── CurrentTool ───────────────────────────────────────────────────────────
export function currentToolToProto(t: CurrentTool): PbCurrentTool {
  return create(CurrentToolSchema, { name: t.name, inputSummary: t.input_summary });
}
export function currentToolFromProto(p: PbCurrentTool): CurrentTool {
  return { name: p.name, input_summary: p.inputSummary };
}

// ─── CurrentBlock ──────────────────────────────────────────────────────────
export function currentBlockToProto(b: CurrentBlock): PbCurrentBlock {
  return create(CurrentBlockSchema, {
    id: BigInt(b.id), command: b.command ?? undefined,
  });
}
export function currentBlockFromProto(p: PbCurrentBlock): CurrentBlock {
  return { id: Number(p.id), command: p.command ?? null };
}

// ─── PermissionRequest ─────────────────────────────────────────────────────
export function permissionRequestToProto(r: PermissionRequest): PbPermissionRequest {
  return create(PermissionRequestSchema, { id: r.id, snippet: r.snippet, options: r.options });
}
export function permissionRequestFromProto(p: PbPermissionRequest): PermissionRequest {
  return { id: p.id, snippet: p.snippet, options: p.options };
}

// ─── SubAgentRow ───────────────────────────────────────────────────────────
export function subAgentRowToProto(s: SubAgentRow): PbSubAgentRow {
  return create(SubAgentRowSchema, {
    parentMessageId: s.parent_message_id,
    childSessionId: s.child_session_id,
    label: s.label,
    status: s.status,
  });
}
export function subAgentRowFromProto(p: PbSubAgentRow): SubAgentRow {
  return {
    parent_message_id: p.parentMessageId,
    child_session_id: p.childSessionId,
    label: p.label,
    status: p.status as SubAgentRow["status"],
  };
}

// ─── AgentState (full, used by Session) ────────────────────────────────────
export function agentStateToProto(a: AgentState): PbAgentState {
  return create(AgentStateSchema, {
    mode: a.mode,
    model: a.model,
    status: a.status,
    tokens: tokensToProto(a.tokens),
    costUsd: a.cost_usd,
    lastMessage: a.last_message ? lastMessageToProto(a.last_message) : undefined,
    currentTool: a.current_tool ? currentToolToProto(a.current_tool) : undefined,
    currentBlock: a.current_block ? currentBlockToProto(a.current_block) : undefined,
    permissionRequest: a.permission_request ? permissionRequestToProto(a.permission_request) : undefined,
    // Tolerate legacy-shape AgentState DB rows that predate sub_agents.
    subAgents: (a.sub_agents ?? []).map(subAgentRowToProto),
    stale: a.stale ?? false,
    sessionFile: a.session_file ?? undefined,
  });
}
// Re-Zod-parses the result. Proto strings are unconstrained but the
// Zod schema enforces enum membership for mode/status/role/etc; a
// drift between worker version and SPA version surfaces here as a
// loud error rather than a silent enum-widened-string in the store.
export function agentStateFromProto(p: PbAgentState): AgentState {
  return AgentStateZ.parse({
    kind: "agent",
    mode: p.mode,
    model: p.model,
    status: p.status,
    tokens: p.tokens ? tokensFromProto(p.tokens) : { in: 0, out: 0, cached: 0 },
    cost_usd: p.costUsd,
    last_message: p.lastMessage ? lastMessageFromProto(p.lastMessage) : null,
    current_tool: p.currentTool ? currentToolFromProto(p.currentTool) : null,
    current_block: p.currentBlock ? currentBlockFromProto(p.currentBlock) : null,
    permission_request: p.permissionRequest ? permissionRequestFromProto(p.permissionRequest) : null,
    sub_agents: p.subAgents.map(subAgentRowFromProto),
    stale: p.stale || undefined,
    session_file: p.sessionFile ?? undefined,
  });
}

// ─── Session ───────────────────────────────────────────────────────────────
export function sessionToProto(s: Session): PbSession {
  return create(SessionSchema, {
    id: s.id,
    workerFp: s.worker_fp,
    channel: s.channel,
    kind: s.kind,
    cwd: s.cwd,
    workspaceId: s.workspace_id ?? undefined,
    status: s.status,
    agent: s.agent ? agentStateToProto(s.agent) : undefined,
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
// Re-Zod-parses at the proto→Zod boundary so brand asserts (asSessionId
// etc.) + enum membership (kind/status) are enforced before the value
// hits the projector. Proto wire is unconstrained string; Zod is the
// in-app schema gate.
export function sessionFromProto(p: PbSession): Session {
  return SessionZ.parse({
    id: p.id,
    worker_fp: p.workerFp,
    channel: p.channel,
    kind: p.kind,
    cwd: p.cwd,
    workspace_id: p.workspaceId ? p.workspaceId : null,
    status: p.status,
    agent: p.agent ? agentStateFromProto(p.agent) : null,
    created_at: Number(p.createdAt),
    closed_at: p.closedAt !== undefined ? Number(p.closedAt) : null,
    custom_title: p.customTitle ?? null,
    // Only carry the field when the wire actually had it, so a branch-less
    // session round-trips identically (git_branch stays absent, not null).
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

// ─── AgentEntry ────────────────────────────────────────────────────────────
// The oneof body case name is the entry `kind`, so the mapping is mechanical.
export function agentEntryToProto(e: AgentEntry): PbAgentEntry {
  const seq = BigInt(e.seq);
  const ts = BigInt(e.ts);
  switch (e.kind) {
    case "user":
    case "assistant":
    case "thinking":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: e.kind,
          value: create(AgentTextEntrySchema, { text: e.text, done: e.done }),
        },
      });
    case "tool":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: "tool",
          value: create(AgentToolEntrySchema, {
            toolCallId: e.tool_call_id,
            name: e.name,
            argsJson: e.args_json,
            status: e.status,
            text: e.text,
            detailsJson: e.details_json,
            intent: e.intent,
          }),
        },
      });
    case "prompt":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: "prompt",
          value: create(AgentPromptEntrySchema, {
            promptId: e.prompt_id,
            kind: e.prompt_kind,
            title: e.title,
            options: e.options,
            allowFreeText: e.allow_free_text,
            state: e.state,
            answer: e.answer,
          }),
        },
      });
    case "notice":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: "notice",
          value: create(AgentNoticeEntrySchema, {
            level: e.level,
            text: e.text,
            detailsJson: e.details_json ?? "",
          }),
        },
      });
    case "todo":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: "todo",
          value: create(AgentTodoEntrySchema, { phasesJson: e.phases_json }),
        },
      });
    case "subagent":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: "subagent",
          value: create(AgentSubagentEntrySchema, {
            subagentId: e.subagent_id,
            name: e.name,
            state: e.state,
            text: e.text,
          }),
        },
      });
    case "image":
      return create(AgentEntrySchema, {
        seq, ts,
        body: {
          case: "image",
          value: create(AgentImageEntrySchema, {
            mediaType: e.media_type,
            dataB64: e.data_b64,
            alt: e.alt,
          }),
        },
      });
  }
}

// Re-Zod-parses, same rationale as agentStateFromProto: proto strings are
// unconstrained, the enums (status/kind/state/level) are gated here.
export function agentEntryFromProto(p: PbAgentEntry): AgentEntry {
  const seq = Number(p.seq);
  const ts = Number(p.ts);
  const b = p.body;
  switch (b.case) {
    case "user":
    case "assistant":
    case "thinking":
      return AgentEntryZ.parse({ kind: b.case, seq, ts, text: b.value.text, done: b.value.done });
    case "tool":
      return AgentEntryZ.parse({
        kind: "tool", seq, ts,
        tool_call_id: b.value.toolCallId,
        name: b.value.name,
        args_json: b.value.argsJson,
        status: b.value.status,
        text: b.value.text,
        details_json: b.value.detailsJson,
        intent: b.value.intent,
      });
    case "prompt":
      return AgentEntryZ.parse({
        kind: "prompt", seq, ts,
        prompt_id: b.value.promptId,
        prompt_kind: b.value.kind,
        title: b.value.title,
        options: b.value.options,
        allow_free_text: b.value.allowFreeText,
        state: b.value.state,
        answer: b.value.answer,
      });
    case "notice":
      return AgentEntryZ.parse({
        kind: "notice", seq, ts,
        level: b.value.level,
        text: b.value.text,
        details_json: b.value.detailsJson ?? "",
      });
    case "todo":
      return AgentEntryZ.parse({ kind: "todo", seq, ts, phases_json: b.value.phasesJson });
    case "subagent":
      return AgentEntryZ.parse({
        kind: "subagent", seq, ts,
        subagent_id: b.value.subagentId,
        name: b.value.name,
        state: b.value.state,
        text: b.value.text,
      });
    case "image":
      return AgentEntryZ.parse({
        kind: "image", seq, ts,
        media_type: b.value.mediaType,
        data_b64: b.value.dataB64,
        alt: b.value.alt,
      });
    default:
      throw new Error("AgentEntry proto has no body");
  }
}

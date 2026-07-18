// Workspace = grouping bucket for sessions. R1.2 throws v1 singleton-
// doc; this is the v2 first-class table shape.

import { z } from "zod";
import { SessionId, WorkerFp, WorkspaceId } from "./brand.ts";

export const Workspace = z.object({
  id: WorkspaceId,
  worker_fp: WorkerFp,        // pinned to one worker
  name: z.string().min(1),
  // Folder path on the worker host. Workspace IS a folder — panes spawned
  // into this workspace inherit this as cwd. Author 2026-06-12:
  // "Server → Workspace (folder path) → Panes".
  folder_path: z.string().min(1),
  color: z.string().nullable(),
  position: z.number().int().nonnegative(),
  version: z.number().int().nonnegative(),  // CAS counter for If-Match
  created_at_ms: z.number().int().positive(),
  updated_at_ms: z.number().int().positive(),
  session_ids: z.array(SessionId),
});
export type Workspace = z.infer<typeof Workspace>;

export const WorkspaceDelta = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("created"), workspace: Workspace }),
  z.object({ kind: z.literal("updated"), workspace: Workspace }),
  z.object({ kind: z.literal("deleted"), id: WorkspaceId }),
  z.object({ kind: z.literal("sessions-set"), id: WorkspaceId, session_ids: z.array(SessionId), version: z.number().int() }),
]);
export type WorkspaceDelta = z.infer<typeof WorkspaceDelta>;

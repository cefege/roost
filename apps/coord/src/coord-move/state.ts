import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { z } from "zod";

export const MOVE_PHASES = [
  "PREPARING_TARGET", "STAGING_WORKERS", "DRAINING_SOURCE", "COPYING_STATE",
  "WAITING_FOR_WORKERS", "COMMITTING", "COMMITTED", "ROLLING_BACK",
  "ROLLED_BACK", "FAILED",
] as const;
export type MovePhase = (typeof MOVE_PHASES)[number];
export type HandoffRole = "SOURCE" | "TARGET";

const handoffSchema = z.object({
  version: z.literal(1),
  handoff_id: z.string().uuid(),
  role: z.enum(["SOURCE", "TARGET"]),
  phase: z.enum(MOVE_PHASES),
  source_url: z.string().url(),
  target_url: z.string().url(),
  target_worker_fp: z.string().min(1),
  expected_worker_fps: z.array(z.string().min(1)),
  commit_acked_worker_fps: z.array(z.string().min(1)),
  expected_coord_kid: z.string().min(1),
  expected_git_sha: z.string().min(1),
  secret_sha256: z.string().regex(/^[a-f0-9]{64}$/),
  secret: z.string().optional(),
  started_at_ms: z.number().int().nonnegative(),
  updated_at_ms: z.number().int().nonnegative(),
  error: z.string().optional(),
}).strict().superRefine((state, context) => {
  if (state.role === "SOURCE" && !state.secret) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "source handoff requires secret" });
  }
  if (state.role === "TARGET" && state.secret !== undefined) {
    context.addIssue({ code: z.ZodIssueCode.custom, message: "target handoff must not contain secret" });
  }
});
export type HandoffState = z.infer<typeof handoffSchema>;

const TERMINAL_PHASES: Record<MovePhase, true | undefined> = {
  PREPARING_TARGET: undefined,
  STAGING_WORKERS: undefined,
  DRAINING_SOURCE: undefined,
  COPYING_STATE: undefined,
  WAITING_FOR_WORKERS: undefined,
  COMMITTING: undefined,
  COMMITTED: true,
  ROLLING_BACK: undefined,
  ROLLED_BACK: true,
  FAILED: true,
};
export function isTerminalPhase(phase: MovePhase): boolean {
  return TERMINAL_PHASES[phase] === true;
}

function fsyncFile(path: string): void {
  const fd = fs.openSync(path, "r");
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}

function fsyncDirectory(path: string): void {
  const fd = fs.openSync(path, "r");
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}

export class HandoffStateStore {
  constructor(readonly path: string) {}

  load(): HandoffState | null {
    try {
      return handoffSchema.parse(JSON.parse(fs.readFileSync(this.path, "utf8")));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw new Error(`invalid coordinator handoff state at ${this.path}: ${(error as Error).message}`);
    }
  }

  write(state: HandoffState): HandoffState {
    const parsed = handoffSchema.parse({ ...state, updated_at_ms: Date.now() });
    const parent = dirname(this.path);
    fs.mkdirSync(parent, { recursive: true, mode: 0o700 });
    const temporary = join(parent, `.${randomUUID()}.coord-handoff.tmp`);
    try {
      fs.writeFileSync(temporary, `${JSON.stringify(parsed)}\n`, { encoding: "utf8", mode: 0o600 });
      fsyncFile(temporary);
      fs.renameSync(temporary, this.path);
      fsyncDirectory(parent);
      return parsed;
    } finally {
      try { fs.unlinkSync(temporary); } catch { /* renamed or absent */ }
    }
  }

  archiveTerminal(state: HandoffState): void {
    if (!isTerminalPhase(state.phase)) throw new Error("cannot archive non-terminal handoff");
    const historyDir = join(dirname(this.path), "handoffs", state.handoff_id);
    fs.mkdirSync(historyDir, { recursive: true, mode: 0o700 });
    const historyPath = join(historyDir, "history.json");
    fs.writeFileSync(historyPath, `${JSON.stringify(state)}\n`, { encoding: "utf8", mode: 0o600 });
    fsyncFile(historyPath);
    fsyncDirectory(historyDir);
  }
}

// Durable coordinator-handoff state: strict zod schema (version 1) plus the
// HandoffStateStore that both SOURCE and TARGET coordinators read/write. The
// schema is versioned and strict because a moved-to coordinator must parse
// what its predecessor wrote — field changes need an upgrade story. Writes
// are atomic and fsynced; writeDurable's onCommitted fires after the rename,
// letting in-memory gates become visible no earlier than the persisted phase.
import { randomUUID } from "node:crypto";
import * as fs from "node:fs";
import { dirname, join } from "node:path";
import { z } from "zod";
import { durableWriteFile } from "@roost/shared/durability";

export const MOVE_PHASES = [
  "PREPARING_TARGET", "STAGING_WORKERS", "DRAINING_SOURCE", "COPYING_STATE",
  "WAITING_FOR_WORKERS", "COMMITTING", "COMMITTED", "ROLLING_BACK",
  "ROLLED_BACK", "FAILED",
] as const;
export type MovePhase = (typeof MOVE_PHASES)[number];

const handoffSchema = z.object({
  version: z.literal(1),
  handoff_id: z.string().uuid(),
  role: z.enum(["SOURCE", "TARGET"]),
  dashboard_id: z.string().min(1),
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

export interface CoordinatorMoveTransaction {
  release(): Promise<void> | void;
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

  /** Synchronous POSIX compatibility path used by fixtures and seed tooling. */
  write(state: HandoffState): HandoffState {
    if (process.platform === "win32") throw new Error("Windows handoff state writes must use writeDurable");
    if (process.platform !== "darwin" && process.platform !== "linux") {
      throw new Error(`unsupported coordinator move platform: ${process.platform}`);
    }
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

  /**
   * `onCommitted` runs synchronously after the replacement is durable and
   * before this promise settles. Lifecycle owners use that boundary to make an
   * in-memory gate transition visible no later than the persisted phase.
   */
  async writeDurable(
    state: HandoffState,
    onCommitted?: (persisted: HandoffState) => void,
  ): Promise<HandoffState> {
    const parsed = handoffSchema.parse({ ...state, updated_at_ms: Date.now() });
    switch (process.platform) {
      case "darwin":
      case "linux": {
        const persisted = this.write(parsed);
        onCommitted?.(persisted);
        return persisted;
      }
      case "win32":
        await durableWriteFile(this.path, `${JSON.stringify(parsed)}\n`, {
          platform: "win32", mode: 0o600, privateDacl: true,
        });
        onCommitted?.(parsed);
        return parsed;
      default:
        throw new Error(`unsupported coordinator move platform: ${process.platform}`);
    }
  }

  async acquireTransaction(): Promise<CoordinatorMoveTransaction> {
    switch (process.platform) {
      case "darwin":
      case "linux":
        return { release() {} };
      case "win32":
        // Windows machine mutation is serialized exclusively inside
        // RoostUpdaterV2. The Coordinator owns only this handoff state.
        return { release() {} };
      default:
        throw new Error(`unsupported coordinator move platform: ${process.platform}`);
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

  async archiveTerminalDurable(state: HandoffState): Promise<void> {
    if (!isTerminalPhase(state.phase)) throw new Error("cannot archive non-terminal handoff");
    switch (process.platform) {
      case "darwin":
      case "linux":
        this.archiveTerminal(state);
        return;
      case "win32": {
        const historyPath = join(dirname(this.path), "handoffs", state.handoff_id, "history.json");
        await durableWriteFile(historyPath, `${JSON.stringify(state)}\n`, {
          platform: "win32", mode: 0o600, privateDacl: true,
        });
        return;
      }
      default:
        throw new Error(`unsupported coordinator move platform: ${process.platform}`);
    }
  }
}

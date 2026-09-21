// Data-only coordinator self-update journal V4 shared by the CLI recovery owner
// and the target coordinator activation gate. Filesystem confinement and
// service-definition identity remain platform-owner responsibilities.

import { z } from "zod";

const FullGitShaSchema = z.string().regex(/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/);
const AbsolutePathSchema = z.string().min(1).max(4_096).refine(
  value => value.startsWith("/") && !/[\r\n\0]/.test(value),
  "path must be an absolute single-line POSIX path",
);
const SafeTimestampSchema = z.number().int().positive().max(Number.MAX_SAFE_INTEGER);

export const CoordinatorDeployPhaseV4Schema = z.enum([
  "prepared",
  "snapshotting",
  "activating",
  "finalizing",
  "rolling-back",
  "prior-restored",
]);
export type CoordinatorDeployPhaseV4 = z.infer<typeof CoordinatorDeployPhaseV4Schema>;

export const CoordinatorDeployJournalV4Schema = z.object({
  schemaVersion: z.literal(4),
  phase: CoordinatorDeployPhaseV4Schema,
  preparedAtMs: SafeTimestampSchema,
  rolloutId: z.string().uuid(),
  priorDefinitionBase64: z.string().min(1).max(2 * 1024 * 1024),
  priorDefinitionMode: z.number().int().min(0).max(0o777),
  priorSha: FullGitShaSchema,
  targetSha: FullGitShaSchema,
  servicePath: AbsolutePathSchema,
  sourceReleasePath: AbsolutePathSchema,
  stagingRepoPath: AbsolutePathSchema,
  stagedReleasePath: AbsolutePathSchema,
  databasePath: AbsolutePathSchema,
  databaseSnapshotPath: AbsolutePathSchema,
  databaseSnapshotSha256: z.string().regex(/^[0-9a-f]{64}$/).nullable(),
}).strict().readonly().superRefine((journal, context) => {
  if ((journal.phase === "prepared" || journal.phase === "snapshotting")
    && journal.databaseSnapshotSha256 !== null) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["databaseSnapshotSha256"],
      message: "snapshot digest must be absent before activation",
    });
  }
  if ((journal.phase === "activating"
      || journal.phase === "finalizing"
      || journal.phase === "rolling-back")
    && journal.databaseSnapshotSha256 === null) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["databaseSnapshotSha256"],
      message: "snapshot digest is required once activation begins",
    });
  }
});
export type CoordinatorDeployJournalV4 = z.infer<typeof CoordinatorDeployJournalV4Schema>;

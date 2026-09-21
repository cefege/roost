// In-process deploy stream frames shared by durable POSIX jobs and the legacy
// signed Windows owner. Connect adapts these to typed protobuf frames.
// It carries structured operation/report snapshots without parsing human output.

import type {
  WorkerUpdateOperation,
  WorkerUpdateReport,
} from "@roost/shared/worker-update-operation";

export type DeployStreamMsg =
  | { kind: "line"; text: string }
  | { kind: "operation"; operation: WorkerUpdateOperation }
  | { kind: "report"; report: WorkerUpdateReport }
  | { kind: "done"; exit: number | null; error?: string };

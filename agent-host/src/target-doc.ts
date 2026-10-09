import type { JsonValue } from "@earendil-works/chord";
import { defineDoc } from "@earendil-works/pi-durable";

export interface RoostTargetState {
  [key: string]: JsonValue;
  worker_fp: string | null;
  worker_label: string | null;
  worker_os: string | null;
  cwd: string | null;
}

export const RoostTarget = defineDoc<RoostTargetState>({
  kind: "roost.target",
  version: 1,
  scope: "conversation",
  history: "latest",
  fork: "initial",
  initial: () => ({ worker_fp: null, worker_label: null, worker_os: null, cwd: null }),
});

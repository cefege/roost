// CoordTarget is the public facade for target-side coordinator relocation.
// It owns one handoff's mutable state while lifecycle and snapshot modules perform transitions.
// Platform-specific service work stays behind the injected runtime and Windows transaction.

import { join } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import { WindowsCoordinatorTargetRelocation } from "./coord-relocation-windows.ts";
import { createDefaultWindowsCoordRuntime } from "./coord-relocation-windows-runtime.ts";
import {
  abort as abortRelocation,
  finalizeCommit as finalizeRelocationCommit,
  prepare as prepareRelocation,
  recoverTransaction as recoverRelocationTransaction,
} from "./coord-target-lifecycle.ts";
import {
  appendSnapshot as appendRelocationSnapshot,
  startSnapshot as startRelocationSnapshot,
} from "./coord-target-snapshot.ts";
import {
  defaultRuntime,
  type CoordTargetContext,
  type CoordTargetPaths,
  type CoordTargetPrepareRequest,
  type CoordTargetRuntime,
  type CoordTargetSnapshotChunk,
  type CoordTargetStartSnapshotRequest,
  type InflightSnapshot,
  type PreparedTarget,
} from "./coord-target-contracts.ts";

export type { CoordTargetPaths, CoordTargetRuntime } from "./coord-target-contracts.ts";

export class CoordTarget {
  #inflight: InflightSnapshot | null = null;
  #prepared: PreparedTarget | null = null;
  readonly #windows: WindowsCoordinatorTargetRelocation | null;
  readonly #context: CoordTargetContext;

  constructor(
    private readonly paths: CoordTargetPaths,
    private readonly runtime: CoordTargetRuntime = defaultRuntime,
  ) {
    switch (runtime.platform) {
      case "darwin":
      case "linux":
        this.#windows = null;
        break;
      case "win32":
        this.#windows = new WindowsCoordinatorTargetRelocation(
          {
            ...paths,
            servicePath: join(roostServiceDir(), "service-definitions.json"),
            currentManifestPath: join(roostServiceDir(), "current.json"),
          },
          runtime.windows ?? createDefaultWindowsCoordRuntime(),
        );
        break;
      default:
        throw new Error(`unsupported coordinator target platform: ${runtime.platform}`);
    }

    const target = this;
    this.#context = {
      paths,
      runtime,
      get inflight() {
        return target.#inflight;
      },
      set inflight(inflight) {
        target.#inflight = inflight;
      },
      get prepared() {
        return target.#prepared;
      },
      set prepared(prepared) {
        target.#prepared = prepared;
      },
      get windows() {
        return target.#windows;
      },
      abort: (handoffId) => target.abort(handoffId),
    };
  }

  recoverTransaction(): Promise<void> {
    return recoverRelocationTransaction.call(this.#context);
  }

  prepare(request: CoordTargetPrepareRequest): Promise<void> {
    return prepareRelocation.call(this.#context, request);
  }

  startSnapshot(request: CoordTargetStartSnapshotRequest): void | Promise<void> {
    return startRelocationSnapshot.call(this.#context, request);
  }

  appendSnapshot(chunk: CoordTargetSnapshotChunk): Promise<void> {
    return appendRelocationSnapshot.call(this.#context, chunk);
  }

  abort(handoffId: string): Promise<void> {
    return abortRelocation.call(this.#context, handoffId);
  }

  finalizeCommit(handoffId: string): Promise<void> {
    return finalizeRelocationCommit.call(this.#context, handoffId);
  }
}

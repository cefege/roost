// Coordinator-to-worker input hold for terminal handoff smoke proofs.
// DelayedFrameStream gives this owner one complete downstream WebSocket payload;
// it recognizes only a bounded DInputRequest and owns release/drop settlement.
// The link disposes it before teardown so no retained input escapes a fixture.

import { fromBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
} from "../../apps/shared/src/gen/roost/v1/worker_transport_pb.ts";

const MAX_INPUT_BYTES = 64 * 1024;

export interface HeldDelayedInput {
  readonly requestId: string;
  release(): void;
  drop(): void;
}

export interface DelayedInputHoldCapture {
  release(): void;
  drop(): void;
}

export interface DelayedInputHoldMatch {
  readonly requestId: string;
}

type PendingInputHold = {
  readonly sessionId: string;
  readonly resolve: (held: HeldDelayedInput) => void;
  readonly reject: (error: Error) => void;
};

type CapturedInputHold = {
  readonly requestId: string;
  readonly release: () => void;
  readonly drop: () => void;
};

/** One link-wide held downstream input. A later frame cannot overtake its queued slot. */
export class DelayedWorkerInputHold {
  #pending: PendingInputHold | null = null;
  #captured: CapturedInputHold | null = null;
  #disposed = false;

  get armed(): boolean {
    return this.#pending !== null;
  }

  holdNextInput(sessionId: string): Promise<HeldDelayedInput> {
    if (this.#disposed) return Promise.reject(new Error("delayed worker link is stopped"));
    if (sessionId.length === 0) return Promise.reject(new Error("delayed input hold requires a session ID"));
    if (this.#pending !== null || this.#captured !== null) {
      return Promise.reject(new Error("delayed worker link already holds an input"));
    }
    return new Promise<HeldDelayedInput>((resolve, reject) => {
      this.#pending = { sessionId, resolve, reject };
    });
  }

  match(payload: Uint8Array): DelayedInputHoldMatch | null {
    const pending = this.#pending;
    if (!pending) return null;
    let frame;
    try {
      frame = fromBinary(CoordWorkerDownSchema, payload);
    } catch {
      return null;
    }
    if (frame.frame.case !== "inputRequest" || frame.frame.value.sessionId !== pending.sessionId) {
      return null;
    }
    if (frame.frame.value.data.byteLength > MAX_INPUT_BYTES) {
      this.#pending = null;
      pending.reject(new Error("delayed worker input exceeds the 64 KiB hold limit"));
      return null;
    }
    return { requestId: frame.frame.value.requestId };
  }

  capture(match: DelayedInputHoldMatch, controls: DelayedInputHoldCapture): boolean {
    const pending = this.#pending;
    if (!pending || this.#captured !== null || this.#disposed) return false;
    this.#pending = null;
    const captured: CapturedInputHold = {
      requestId: match.requestId,
      release: controls.release,
      drop: controls.drop,
    };
    this.#captured = captured;
    pending.resolve({
      requestId: captured.requestId,
      release: () => { this.settle(captured, "release"); },
      drop: () => { this.settle(captured, "drop"); },
    });
    return true;
  }

  abandonCapture(controls: DelayedInputHoldCapture): void {
    const captured = this.#captured;
    if (!captured || captured.release !== controls.release || captured.drop !== controls.drop) return;
    this.#captured = null;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    const pending = this.#pending;
    this.#pending = null;
    pending?.reject(new Error("delayed worker link stopped before input arrived"));
    const captured = this.#captured;
    this.#captured = null;
    captured?.drop();
  }

  private settle(captured: CapturedInputHold, action: "release" | "drop"): void {
    if (this.#captured !== captured) return;
    this.#captured = null;
    if (action === "release") captured.release();
    else captured.drop();
  }
}

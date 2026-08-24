// Type surface for the per-channel terminal streaming state machine: what
// each channel's delivery stream records (baseline readiness, live resize
// capture, snapshot cursor, core validity) and the closed set of failure
// kinds a control request can fail with. Types only — behavior lives in
// session-terminal-txn / -control.
import type { PbCellGridChunk, PbCellGridFrame } from "@roost/shared/proto/cell_pb";

export type TerminalStreamFailure =
	| "retryable_pre_write"
	| "session_not_live"
	| "invalid_request"
	| "core_failed"
	| "ambiguous_boundary";

export type TerminalStreamWritePhase = "pre_write" | "written" | "unknown";
export type WorkerTerminalStreamResult =
	| {
		status: "committed";
		streamId: string;
		enabled: boolean;
		channelResizeSeq: number;
		cols: number;
		rows: number;
		resized: boolean;
		phase: TerminalStreamWritePhase;
	}
	| {
		status: "rejected" | "ambiguous";
		streamId: string;
		enabled: boolean;
		channelResizeSeq: number;
		cols: number;
		rows: number;
		failure: TerminalStreamFailure;
		reason: string;
		phase: TerminalStreamWritePhase;
	};

export interface TerminalSnapshotCursor {
	readonly streamId: string;
	readonly snapshotId: string;
	readonly seq: bigint;
	readonly parts: readonly TerminalSnapshotPart[];
	nextPart: number;
}

export type TerminalSnapshotPart =
	| { readonly kind: "frame"; readonly value: PbCellGridFrame }
	| { readonly kind: "chunk"; readonly value: PbCellGridChunk };

/** The only worker-side ownership record for terminal delivery on one channel. */
export interface TerminalStreamState {
	readonly streamId: string;
	readonly enabled: boolean;
	readonly cols: number;
	readonly rows: number;
	/** Monotonic worker receipt identity; queued work must still own this object. */
	readonly version: number;
	/** False from stream install/snapshot request until the final full part is sent. */
	baselineReady: boolean;
	/** A trapped resize makes every later PTY byte take the recovery-record lane. */
	coreValid: boolean;
	/** Dirty work observed while the full cursor was blocked. */
	baselineDirty: boolean;
	snapshotCursor: TerminalSnapshotCursor | null;
	resizeCapture: LiveResizeCapture | null;
	/** Settles when a complete immutable full transfer has been installed. */
	baselineInstalled: Promise<void>;
	resolveBaselineInstalled: () => void;
	operation?: Promise<WorkerTerminalStreamResult>;
}

/** Bounded raw-ring cursor used only while a sequenced live resize is unresolved. */
export interface LiveResizeCapture {
	readonly streamId: string;
	readonly resizeSeq: number;
	readonly installSeq: number;
	readonly fromCols: number;
	readonly fromRows: number;
	readonly toCols: number;
	readonly toRows: number;
	readonly queryCarry: Uint8Array;
	capturedBytes: number;
	capturedChunks: number;
	boundarySeq: number;
	boundaryApplied: boolean;
	failedReason: string | null;
}

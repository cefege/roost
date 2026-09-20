// Large cell frame fixture for local-terminal sink overflow tests.
// The payload crosses the loopback backpressure bound in two sends without
// requiring thousands of incidental frames.

import { create } from "@bufbuild/protobuf";
import {
	PbCellGridFrameSchema,
	PbCellRowSchema,
	PbCellSpanSchema,
} from "@roost/shared/proto/cell_pb";
import { randomUUID } from "node:crypto";

export function oversizedLocalTerminalDelta(sessionId: string) {
	return create(PbCellGridFrameSchema, {
		sessionId,
		streamId: randomUUID(),
		seq: 2n,
		baseSeq: 1n,
		full: false,
		viewportRows: [create(PbCellRowSchema, {
			index: 0,
			spans: [create(PbCellSpanSchema, { text: "x".repeat(3_000_000) })],
		})],
	});
}

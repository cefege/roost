// LocalTerminal protobuf delivery helpers shared by loopback and peer ports.
// LocalTerminalSockets chooses lane and close policy; this helper only encodes a
// whole server frame and treats carrier queue ownership as a cell-delivery ACK.
// It never retains terminal bytes after the port accepts them.

import { create, toBinary } from "@bufbuild/protobuf";
import {
	LocalTerminalServerFrameSchema,
	type LocalTerminalServerFrame,
} from "@roost/shared/proto/local_terminal_pb";
import {
	InputAcceptedSchema,
	InputAmbiguousSchema,
	InputRejectedSchema,
	type InputCommand,
} from "@roost/shared/proto/sync_pb";
import type { TerminalPeerPacketLane } from "@roost/shared/terminal-peer";
import type { WorkerInputResult } from "./session-terminal-control.ts";
import type { TerminalPacketPort, TerminalPacketSendResult } from "./terminal-packet-port.ts";

export type LocalTerminalControlSender = (frame: LocalTerminalServerFrame["frame"]) => void;

export function sendLocalTerminalFrame(
	port: TerminalPacketPort,
	frame: LocalTerminalServerFrame["frame"],
	lane: TerminalPeerPacketLane,
	onRefused: ((reason: string) => void) | undefined,
): TerminalPacketSendResult {
	let bytes: Uint8Array;
	try {
		bytes = toBinary(LocalTerminalServerFrameSchema, create(LocalTerminalServerFrameSchema, { frame }));
	} catch {
		onRefused?.("terminal frame encoding failed");
		return "refused";
	}
	const result = port.send(bytes, lane);
	if (result === "refused") onRefused?.("local control delivery refused");
	return result;
}

export function sendLocalTerminalInputResult(
	sendControl: LocalTerminalControlSender,
	generation: bigint,
	command: InputCommand,
	result: WorkerInputResult,
): void {
	const common = {
		sessionId: command.sessionId,
		inputSeq: command.inputSeq,
		domainGeneration: generation,
	};
	if (result.status === "accepted") {
		sendControl({
			case: "inputAccepted",
			value: create(InputAcceptedSchema, { ...common, writtenBytes: result.writtenBytes }),
		});
		return;
	}
	if (result.status === "rejected") {
		sendControl({
			case: "inputRejected",
			value: create(InputRejectedSchema, { ...common, reason: result.reason }),
		});
		return;
	}
	sendControl({
		case: "inputAmbiguous",
		value: create(InputAmbiguousSchema, {
			...common,
			writtenBytes: result.writtenBytes,
			reason: result.reason,
		}),
	});
}

export function localTerminalCellDelivery(
	result: TerminalPacketSendResult,
	close: () => void,
): "sent" | "overflow" {
	if (result === "accepted" || result === "backpressured") return "sent";
	close();
	return "overflow";
}

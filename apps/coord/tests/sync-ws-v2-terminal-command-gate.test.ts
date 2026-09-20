// Pins coord's terminal command gate: a refused `input` command must receive a
// definite inputRejected reply so the browser never waits out its result
// deadline and reports possible data loss.

import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  InputCommandSchema,
  SyncClientFrameSchema,
  type SyncClientFrame,
  TerminalInputRouteClaimSchema,
} from "@roost/shared/proto/sync_pb";
import { makeSyncV2CommandHandler } from "../src/connect/sync-ws-v2-commands.ts";
import {
  decodedFrames,
  makeHarness,
  SESSION_A,
  type SchedulerHarness,
} from "./sync-ws-v2-scheduler-harness.ts";

interface GateFixture {
  harness: SchedulerHarness;
  handleV2Command(frame: SyncClientFrame): void;
  commands: string[];
}

function makeGate(terminalReady: boolean): GateFixture {
  const harness = makeHarness("viewer", terminalReady, {});
  const commands: string[] = [];
  const handler = makeSyncV2CommandHandler({
    sendV2ControlFrame: harness.scheduler.sendV2ControlFrame,
    resetV2Domain: harness.scheduler.resetV2Domain,
    scheduleV2: harness.scheduler.scheduleV2,
    onV2Command: (context) => commands.push(context.command.case),
  });
  return {
    harness,
    handleV2Command: (frame) => handler.handleV2Command(harness.ws, frame),
    commands,
  };
}

function inputFrame(gate: GateFixture, domainGeneration: bigint): SyncClientFrame {
  return create(SyncClientFrameSchema, {
    socketId: gate.harness.socket.data.v2!.socketId,
    command: {
      case: "input",
      value: create(InputCommandSchema, {
        sessionId: SESSION_A,
        inputSeq: 7n,
        data: new Uint8Array([65]),
        domainGeneration,
      }),
    },
  });
}

function resyncFrame(gate: GateFixture, domainGeneration: bigint): SyncClientFrame {
  return create(SyncClientFrameSchema, {
    socketId: gate.harness.socket.data.v2!.socketId,
    command: {
      case: "terminalResync",
      value: { sessionId: SESSION_A, domainGeneration },
    },
  });
}

function routeClaimFrame(gate: GateFixture, domainGeneration: bigint): SyncClientFrame {
  return create(SyncClientFrameSchema, {
    socketId: gate.harness.socket.data.v2!.socketId,
    command: {
      case: "inputRouteClaim",
      value: create(TerminalInputRouteClaimSchema, {
        requestId: "route-command-gate",
        sessionId: SESSION_A,
        revision: 1n,
        domainGeneration,
        workerEpoch: "worker-epoch",
      }),
    },
  });
}

function onlyRejection(gate: GateFixture): {
  sessionId: string;
  inputSeq: bigint;
  domainGeneration: bigint;
  reason: string;
} {
  const frames = decodedFrames(gate.harness.socket);
  expect(frames).toHaveLength(1);
  const frame = frames[0]?.frame;
  if (frame?.case !== "inputRejected") throw new Error(`expected inputRejected, got ${frame?.case}`);
  return frame.value;
}

describe("Sync v2 terminal command gate", () => {
  test("an input command for a resubscribing terminal domain is rejected, not dropped", () => {
    const gate = makeGate(false);
    gate.handleV2Command(inputFrame(gate, gate.harness.terminal.generation));
    const rejection = onlyRejection(gate);
    expect(rejection.sessionId).toBe(SESSION_A);
    expect(rejection.inputSeq).toBe(7n);
    expect(rejection.domainGeneration).toBe(gate.harness.terminal.generation);
    expect(rejection.reason).toContain("resubscribing");
    expect(gate.commands).toEqual([]);
  });
  test("an input command under a stale domain generation is rejected with its own generation echoed", () => {
    const gate = makeGate(true);
    const stale = gate.harness.terminal.generation - 1n;
    gate.handleV2Command(inputFrame(gate, stale));
    const rejection = onlyRejection(gate);
    expect(rejection.domainGeneration).toBe(stale);
    expect(rejection.reason).toContain("generation was reset");
    expect(gate.commands).toEqual([]);
  });

  test("a read-only socket's input command is rejected instead of ignored", () => {
    const gate = makeGate(true);
    gate.harness.socket.data.readOnly = true;
    gate.handleV2Command(inputFrame(gate, gate.harness.terminal.generation));
    expect(onlyRejection(gate).reason).toContain("cannot write terminal input");
    expect(gate.commands).toEqual([]);
  });

  test("a read-only socket receives a definite route-claim refusal", () => {
    const gate = makeGate(true);
    gate.harness.socket.data.readOnly = true;
    gate.handleV2Command(routeClaimFrame(gate, gate.harness.terminal.generation));
    const frames = decodedFrames(gate.harness.socket);
    expect(frames).toHaveLength(1);
    const frame = frames[0]?.frame;
    if (frame?.case !== "inputRouteResult") throw new Error(`expected inputRouteResult, got ${frame?.case}`);
    expect(frame.value).toMatchObject({
      requestId: "route-command-gate",
      accepted: false,
      reason: "this Sync socket cannot write terminal input",
    });
    expect(gate.commands).toEqual([]);
  });

  test("a refused terminalResync still gets no reply", () => {
    const gate = makeGate(false);
    gate.handleV2Command(resyncFrame(gate, gate.harness.terminal.generation));
    expect(decodedFrames(gate.harness.socket)).toHaveLength(0);
    expect(gate.commands).toEqual([]);
  });

  test("an admissible input command reaches the command consumer unanswered", () => {
    const gate = makeGate(true);
    gate.handleV2Command(inputFrame(gate, gate.harness.terminal.generation));
    expect(gate.commands).toEqual(["input"]);
    expect(decodedFrames(gate.harness.socket)).toHaveLength(0);
  });
});

// Pins the public guarded-prompt RPC and its dedicated worker transport frame.
// Tests cover every fence, optional wait outcome, and enum value while
// retaining the existing raw terminal-input byte contracts unchanged.

import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AgentPromptInputOutcome,
  AgentPromptWaitOutcome,
  CoordinatorService,
  SessionsInputRequestSchema,
  SessionsPromptRequestSchema,
  SessionsPromptResponseSchema,
} from "../src/gen/roost/v1/coordinator_pb.ts";
import {
  CoordWorkerUpSchema,
  CoordWorkerDownSchema,
  DAgentPromptSchema,
  DInputRequestSchema,
  TerminalInputStatus,
  TerminalWritePhase,
  WInputResultSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";

const sessionId = "11111111-1111-4111-8111-111111111111";
const statusEpoch = "22222222-2222-4222-8222-222222222222";
const occupantId = "33333333-3333-4333-8333-333333333333";

describe("guarded agent prompt protobuf contract", () => {
  test("exposes SessionsPrompt with separate input and wait outcomes", () => {
    expect(CoordinatorService.methods.map((method) => method.localName)).toContain(
      "sessionsPrompt",
    );
    expect(AgentPromptInputOutcome).toMatchObject({
      UNSPECIFIED: 0,
      ACCEPTED: 1,
      REJECTED: 2,
      AMBIGUOUS: 3,
    });
    expect(AgentPromptWaitOutcome).toMatchObject({
      UNSPECIFIED: 0,
      MATCHED: 1,
      TIMED_OUT: 2,
      OCCUPANT_CHANGED: 3,
      SESSION_CLOSED: 4,
    });
  });

  test("round-trips exact identity, safe revision, text, and optional wait", () => {
    const request = create(SessionsPromptRequestSchema, {
      sessionId,
      expectedStatusEpoch: statusEpoch,
      expectedOccupantId: occupantId,
      expectedRevision: BigInt(Number.MAX_SAFE_INTEGER),
      text: "continue λ",
      waitStates: ["idle", "blocked"],
      waitTimeoutMs: 300_000,
    });
    expect(fromBinary(
      SessionsPromptRequestSchema,
      toBinary(SessionsPromptRequestSchema, request),
    )).toMatchObject({
      sessionId,
      expectedStatusEpoch: statusEpoch,
      expectedOccupantId: occupantId,
      expectedRevision: BigInt(Number.MAX_SAFE_INTEGER),
      text: "continue λ",
      waitStates: ["idle", "blocked"],
      waitTimeoutMs: 300_000,
    });

    const withoutWait = create(SessionsPromptRequestSchema, {
      sessionId,
      expectedStatusEpoch: statusEpoch,
      expectedOccupantId: occupantId,
      expectedRevision: 7n,
      text: "continue",
    });
    const withoutWaitResult = fromBinary(
      SessionsPromptRequestSchema,
      toBinary(SessionsPromptRequestSchema, withoutWait),
    );
    expect(withoutWaitResult.waitStates).toEqual([]);
    expect(withoutWaitResult.waitTimeoutMs).toBeUndefined();
  });

  test("round-trips input truth independently from optional wait outcomes", () => {
    for (const [inputOutcome, waitOutcome] of [
      [AgentPromptInputOutcome.ACCEPTED, AgentPromptWaitOutcome.MATCHED],
      [AgentPromptInputOutcome.AMBIGUOUS, AgentPromptWaitOutcome.OCCUPANT_CHANGED],
      [AgentPromptInputOutcome.ACCEPTED, AgentPromptWaitOutcome.TIMED_OUT],
      [AgentPromptInputOutcome.AMBIGUOUS, AgentPromptWaitOutcome.SESSION_CLOSED],
    ] as const) {
      const response = create(SessionsPromptResponseSchema, {
        inputOutcome,
        writtenBytes: 9,
        reason: "",
        waitOutcome,
      });
      expect(fromBinary(
        SessionsPromptResponseSchema,
        toBinary(SessionsPromptResponseSchema, response),
      )).toMatchObject({ inputOutcome, waitOutcome });
    }

    const rejected = create(SessionsPromptResponseSchema, {
      inputOutcome: AgentPromptInputOutcome.REJECTED,
      writtenBytes: 0,
      reason: "fence changed",
    });
    const rejectedResult = fromBinary(
      SessionsPromptResponseSchema,
      toBinary(SessionsPromptResponseSchema, rejected),
    );
    expect(rejectedResult).toMatchObject({
      inputOutcome: AgentPromptInputOutcome.REJECTED,
      writtenBytes: 0,
      reason: "fence changed",
    });
    expect(rejectedResult.waitOutcome).toBeUndefined();

    const acceptedWithoutWait = create(SessionsPromptResponseSchema, {
      inputOutcome: AgentPromptInputOutcome.ACCEPTED,
      writtenBytes: 9,
      reason: "",
    });
    expect(fromBinary(
      SessionsPromptResponseSchema,
      toBinary(SessionsPromptResponseSchema, acceptedWithoutWait),
    ).waitOutcome).toBeUndefined();
  });

  test("round-trips DAgentPrompt as downstream tag 16", () => {
    const prompt = create(DAgentPromptSchema, {
      requestId: "prompt-request",
      sessionId,
      inputSeq: 41n,
      expectedStatusEpoch: statusEpoch,
      expectedOccupantId: occupantId,
      expectedRevision: 42n,
      text: "continue",
      budgetMs: 4_500,
    });
    const encoded = toBinary(CoordWorkerDownSchema, create(CoordWorkerDownSchema, {
      frame: { case: "agentPrompt", value: prompt },
    }));
    expect(encoded[0]).toBe(0x82);
    expect(encoded[1]).toBe(0x01);
    expect(fromBinary(CoordWorkerDownSchema, encoded).frame).toMatchObject({
      case: "agentPrompt",
      value: {
        requestId: "prompt-request",
        sessionId,
        inputSeq: 41n,
        expectedStatusEpoch: statusEpoch,
        expectedOccupantId: occupantId,
        expectedRevision: 42n,
        text: "continue",
        budgetMs: 4_500,
      },
    });
  });

  test("retains WInputResult as the upstream write truth", () => {
    const result = create(WInputResultSchema, {
      requestId: "prompt-request",
      sessionId,
      inputSeq: 41n,
      status: TerminalInputStatus.ACCEPTED,
      writtenBytes: 9,
      reason: "",
      phase: TerminalWritePhase.WRITTEN,
    });
    const upstream = create(CoordWorkerUpSchema, {
      frame: { case: "inputResult", value: result },
    });
    expect(fromBinary(
      CoordWorkerUpSchema,
      toBinary(CoordWorkerUpSchema, upstream),
    ).frame).toMatchObject({
      case: "inputResult",
      value: {
        requestId: "prompt-request",
        inputSeq: 41n,
        status: TerminalInputStatus.ACCEPTED,
        writtenBytes: 9,
        phase: TerminalWritePhase.WRITTEN,
      },
    });
  });

  test("keeps raw SessionsInput and DInputRequest payloads as bytes", () => {
    const data = new Uint8Array([0, 13, 255]);
    const publicInput = create(SessionsInputRequestSchema, { sessionId, data });
    const downstreamInput = create(DInputRequestSchema, {
      requestId: "raw-input",
      sessionId,
      inputSeq: 1n,
      data,
      budgetMs: 1_000,
    });
    expect(fromBinary(
      SessionsInputRequestSchema,
      toBinary(SessionsInputRequestSchema, publicInput),
    ).data).toEqual(data);
    expect(fromBinary(
      DInputRequestSchema,
      toBinary(DInputRequestSchema, downstreamInput),
    ).data).toEqual(data);
  });
});

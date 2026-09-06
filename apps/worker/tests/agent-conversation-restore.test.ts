// Pins the worker-owned OMP resume plan (`omp --resume=<ref>`), its POSIX
// quoting, reference validation, per-pass dedupe, and the one-batch keeper
// acknowledgement truth model. No integration text becomes executable syntax.

import { afterEach, describe, expect, test, vi } from "bun:test";
import type { AgentConversationReferenceV1 } from "@roost/shared/agent-conversation-reference";
import { log } from "@roost/shared/log";
import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  OMP_CONVERSATION_RESUME_DESCRIPTOR_V1,
  conversationRestoreDedupeKey,
  materializeOmpConversationRestoreInput,
  restoreAgentConversationAfterRespawn,
} from "../src/agent-conversation-restore.ts";
import { MuxFrameType } from "../src/keeper/protocol.ts";
import { installFakeKeeper } from "./keeper-fake-pool.ts";
import {
  CHANNEL_ID,
  cleanupStreamHarnesses,
  makeHarness,
  SESSION_ID,
  trackKeeper,
} from "./terminal-stream-state-harness.ts";

const UTF8_DECODER = new TextDecoder();

function reference(
  kind: AgentConversationReferenceV1["kind"],
  value: string,
): AgentConversationReferenceV1 {
  return { schema_version: 1, agent_id: "omp", kind, value };
}

function commandWithoutTerminator(
  agentReference: AgentConversationReferenceV1,
): string {
  const payload = materializeOmpConversationRestoreInput(
    agentReference,
    "linux",
  );
  expect(payload).not.toBeNull();
  expect(payload!.at(-1)).toBe(0x0d);
  return UTF8_DECODER.decode(payload!.subarray(0, -1));
}

afterEach(() => {
  cleanupStreamHarnesses();
  vi.restoreAllMocks();
});

describe("OMP conversation restore materialization", () => {
  test("descriptor pins the official executable and option form", () => {
    expect(OMP_CONVERSATION_RESUME_DESCRIPTOR_V1).toEqual({
      schema_version: 1,
      agent_id: "omp",
      executable: "omp",
      fixed_option_prefix: "--resume=",
      reference_kinds: ["id", "path"],
      platforms: ["darwin", "linux"],
    });
  });

  for (const agentReference of [
    reference("id", "01J8OMP-prefix"),
    reference(
      "path",
      "/tmp/omp 'session' $(printf injected); `printf nope`.jsonl",
    ),
  ]) {
    test(`${agentReference.kind} reference stays one opaque argv element`, () => {
      if (process.platform === "win32") return;
      const command = commandWithoutTerminator(agentReference);
      expect(command).toBe(
        [
          posixShellQuote("omp"),
          posixShellQuote(`--resume=${agentReference.value}`),
        ].join(" "),
      );
      const shell = Bun.spawnSync([
        "sh",
        "-c",
        `omp() { printf '%s\\0' "$#" "$1"; }\n${command}`,
      ]);
      expect(shell.exitCode).toBe(0);
      expect(shell.stderr.toString()).toBe("");
      expect(shell.stdout.toString().split("\0")).toEqual([
        "1",
        `--resume=${agentReference.value}`,
        "",
      ]);
    });
  }

  test("Windows has no restore materialization", () => {
    expect(materializeOmpConversationRestoreInput(
      reference("id", "opaque"),
      "win32",
    )).toBeNull();
  });

  test("control characters, relative paths, and oversized refs are unsupported", () => {
    for (const control of ["\u0000", "\u0003", "\u0009", "\u000a", "\u000d", "\u007f"]) {
      expect(materializeOmpConversationRestoreInput(
        reference("id", `prefix${control}suffix`),
        "linux",
      )).toBeNull();
    }
    expect(materializeOmpConversationRestoreInput(
      reference("path", "relative/session.jsonl"),
      "linux",
    )).toBeNull();
    expect(materializeOmpConversationRestoreInput(
      reference("id", "x".repeat(513)),
      "linux",
    )).toBeNull();
    expect(materializeOmpConversationRestoreInput(
      reference("path", `/${"x".repeat(4096)}`),
      "linux",
    )).toBeNull();
    expect(materializeOmpConversationRestoreInput(
      reference("path", "/tmp/session.jsonl"),
      "linux",
    )).not.toBeNull();
  });
});

describe("OMP conversation restore input", () => {
  for (const scenario of [
    { name: "accepted", kind: "ack" },
    { name: "rejected", kind: "reject" },
    { name: "ambiguous", kind: "ambiguous" },
  ] as const) {
    test(`${scenario.name} keeper result is terminal after one batch`, async () => {
      const keeper = trackKeeper(installFakeKeeper());
      const harness = await makeHarness();
      const agentReference = reference("path", "/private/opaque-secret.jsonl");
      const info = vi.spyOn(log, "info").mockImplementation(() => {});
      const warn = vi.spyOn(log, "warn").mockImplementation(() => {});
      const resultPromise = restoreAgentConversationAfterRespawn({
        enabled: true,
        sessionMgr: harness.manager,
        platform: "linux",
      }, String(SESSION_ID), agentReference);
      const write = await keeper.waitForWrite(MuxFrameType.PtyInRequest);
      expect(write.seq).not.toBeNull();
      expect(write.bytes).toEqual(
        materializeOmpConversationRestoreInput(agentReference, "linux"),
      );
      if (scenario.kind === "ack") {
        keeper.inputAck(CHANNEL_ID, write.seq!, write.bytes!.byteLength);
      } else if (scenario.kind === "reject") {
        keeper.inputReject(CHANNEL_ID, write.seq!, "queue_full");
      } else {
        keeper.inputAmbiguous(CHANNEL_ID, write.seq!, {
          writtenBytes: 1,
          reason: "write_error",
        });
      }
      const result = await resultPromise;
      if (scenario.kind === "ack") {
        expect(result).toEqual({
          status: "accepted",
          writtenBytes: write.bytes!.byteLength,
        });
      } else if (scenario.kind === "reject") {
        expect(result).toEqual({
          status: "rejected",
          writtenBytes: 0,
          reason: "queue_full",
        });
      } else {
        expect(result).toEqual({
          status: "ambiguous",
          writtenBytes: 1,
          reason: "write_error",
        });
      }
      expect(keeper.writes.filter(
        (candidate) => candidate.type === MuxFrameType.PtyInRequest,
      )).toHaveLength(1);
      const transitions = [...info.mock.calls, ...warn.mock.calls].filter(
        (call) => call[1] === "agent_conversation_restore_transition",
      );
      expect(transitions).toHaveLength(1);
      const serializedTransition = JSON.stringify(transitions[0]);
      expect(serializedTransition).not.toContain(agentReference.value);
      expect(serializedTransition).not.toContain("--resume=");
    });
  }

  test("disabled, missing, and unsupported restores write zero input", async () => {
    const keeper = trackKeeper(installFakeKeeper());
    const harness = await makeHarness();
    const agentReference = reference("id", "opaque");
    expect(await restoreAgentConversationAfterRespawn({
      enabled: false,
      sessionMgr: harness.manager,
      platform: "linux",
    }, String(SESSION_ID), agentReference)).toEqual({
      status: "skipped",
      reason: "disabled",
    });
    expect(await restoreAgentConversationAfterRespawn({
      enabled: true,
      sessionMgr: harness.manager,
      platform: "linux",
    }, String(SESSION_ID), null)).toEqual({
      status: "skipped",
      reason: "missing_reference",
    });
    expect(await restoreAgentConversationAfterRespawn({
      enabled: true,
      sessionMgr: harness.manager,
      platform: "win32",
    }, String(SESSION_ID), agentReference)).toEqual({
      status: "skipped",
      reason: "unsupported",
    });
    expect(await restoreAgentConversationAfterRespawn({
      enabled: true,
      sessionMgr: harness.manager,
      platform: "linux",
    }, String(SESSION_ID), reference("id", "opaque\u0015suffix"))).toEqual({
      status: "skipped",
      reason: "unsupported",
    });
    expect(keeper.writes.filter(
      (candidate) => candidate.type === MuxFrameType.PtyInRequest,
    )).toHaveLength(0);
  });

  test("a reference already claimed in this pass never resumes twice", async () => {
    const keeper = trackKeeper(installFakeKeeper());
    const harness = await makeHarness();
    const agentReference = reference("path", "/private/shared.jsonl");
    const resumedReferenceKeys = new Set<string>([
      conversationRestoreDedupeKey(agentReference),
    ]);

    expect(await restoreAgentConversationAfterRespawn({
      enabled: true,
      sessionMgr: harness.manager,
      platform: "linux",
      resumedReferenceKeys,
    }, String(SESSION_ID), agentReference)).toEqual({
      status: "skipped",
      reason: "duplicate",
    });
    expect(keeper.writes.filter(
      (candidate) => candidate.type === MuxFrameType.PtyInRequest,
    )).toHaveLength(0);
  });
});

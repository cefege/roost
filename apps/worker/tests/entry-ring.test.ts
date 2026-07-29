import { expect, test } from "bun:test";
import { toBinary } from "@bufbuild/protobuf";
import { AgentEntriesFrameSchema, type AgentEntriesFrame } from "@roost/shared/proto/sync_pb";
import { AGENT_ENTRY_CAPS, clampText } from "@roost/shared/wire/agent-entry";
import { agentEntryFromProto } from "@roost/shared/wire/agent-proto";
import { asSessionId } from "@roost/shared/wire";
import { AgentEntryRing } from "../src/agent/entry-ring.ts";

test("maximum image entries split into transport-safe singleton frames", () => {
  const frames: AgentEntriesFrame[] = [];
  const ring = new AgentEntryRing(
    asSessionId("11111111-1111-4111-8111-111111111111"),
    (frame) => frames.push(frame),
  );
  const data = "A".repeat(AGENT_ENTRY_CAPS.imageBytes);
  for (const seq of [1, 2]) {
    ring.append({
      kind: "image",
      seq,
      ts: seq,
      media_type: "image/png",
      data_b64: data,
      alt: "screenshot",
    });
  }

  ring.flushNow();

  expect(frames).toHaveLength(2);
  expect(frames.every((frame) => frame.entries.length === 1)).toBe(true);
  for (const frame of frames) {
    expect(toBinary(AgentEntriesFrameSchema, frame).byteLength).toBeLessThanOrEqual(
      AGENT_ENTRY_CAPS.framePayload,
    );
  }
});

test("oversized multibyte tool singleton becomes a bounded error notice", () => {
  const frames: AgentEntriesFrame[] = [];
  const ring = new AgentEntryRing(
    asSessionId("22222222-2222-4222-8222-222222222222"),
    (frame) => frames.push(frame),
  );
  const multibyte = "€".repeat(AGENT_ENTRY_CAPS.toolDetails);
  ring.append({
    kind: "tool",
    seq: 1,
    ts: 1,
    tool_call_id: "tool-1",
    name: "write",
    args_json: multibyte,
    status: "ok",
    text: multibyte,
    details_json: multibyte,
    intent: "",
  });

  ring.flushNow();

  expect(frames).toHaveLength(1);
  const frame = frames[0];
  if (!frame) throw new Error("expected one frame");
  expect(toBinary(AgentEntriesFrameSchema, frame).byteLength).toBeLessThanOrEqual(
    AGENT_ENTRY_CAPS.framePayload,
  );
  const entry = agentEntryFromProto(frame.entries[0]!);
  expect(entry.kind).toBe("notice");
  if (entry.kind !== "notice") throw new Error("expected oversized-entry notice");
  expect(entry.level).toBe("error");
  expect(entry.text).toContain("encoded payload exceeded");
});

test("maximum multibyte assistant text remains intact within one frame", () => {
  const frames: AgentEntriesFrame[] = [];
  const ring = new AgentEntryRing(
    asSessionId("33333333-3333-4333-8333-333333333333"),
    (frame) => frames.push(frame),
  );
  const text = "界".repeat(AGENT_ENTRY_CAPS.text);
  ring.append({ kind: "assistant", seq: 1, ts: 1, text, done: true });

  ring.flushNow();

  expect(frames).toHaveLength(1);
  const frame = frames[0];
  if (!frame) throw new Error("expected one frame");
  expect(toBinary(AgentEntriesFrameSchema, frame).byteLength).toBeLessThanOrEqual(
    AGENT_ENTRY_CAPS.framePayload,
  );
  const entry = agentEntryFromProto(frame.entries[0]!);
  if (entry.kind !== "assistant") throw new Error("expected assistant entry");
  expect(entry.text).toBe(text);
});

test("notice details survive protobuf framing", () => {
  const frames: AgentEntriesFrame[] = [];
  const ring = new AgentEntryRing(
    asSessionId("44444444-4444-4444-8444-444444444444"),
    (frame) => frames.push(frame),
  );
  const details_json = JSON.stringify({ file: "src/index.ts", diagnostics: ["broken import"] });
  ring.append({
    kind: "notice",
    seq: 1,
    ts: 1,
    level: "warn",
    text: "Late diagnostics",
    details_json,
  });

  ring.flushNow();

  const entry = agentEntryFromProto(frames[0]!.entries[0]!);
  if (entry.kind !== "notice") throw new Error("expected notice entry");
  expect(entry.details_json).toBe(details_json);
});

test("text truncation marker stays inside the requested cap", () => {
  expect(clampText("abcdef", 5)).toHaveLength(5);
  expect(clampText("abcdef", 1)).toHaveLength(1);
  expect(clampText("abcdef", 0)).toBe("");
});

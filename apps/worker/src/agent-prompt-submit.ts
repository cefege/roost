// Owns how an admitted agent prompt reaches the PTY: the encoded text and the
// CR that submits it, as two separately acknowledged keeper writes on the
// admission ticket agent-prompt-control.ts already holds. The keeper's
// acknowledgements are the only truth about what landed.

import { CR_BYTES } from "@roost/shared/terminal-input";
import { getMultiplexedPool } from "./keeper/multiplexed-client.ts";
import type { WorkerInputResult } from "./session-terminal-control.ts";

/** Gap between the prompt text and its CR. Agents that debounce bracketed-paste
 *  assembly see a fused `…ESC[201~\r` as one burst and keep the text as an
 *  unsubmitted draft, so the CR only goes out once the paste has settled. */
export const PROMPT_SUBMIT_DELAY_MS = 300;

type KeeperWriteOutcome =
  | { kind: "acked"; writtenBytes: number }
  | { kind: "unwritten"; reason: string }
  | { kind: "uncertain"; writtenBytes: number; reason: string };

/** Write the prompt text, then the CR alone once the paste has settled. The
 *  caller must still hold the input lane across both writes: another writer's
 *  bytes landing between them would be submitted along with the prompt.
 *  `accepted` means BOTH writes were acknowledged; an acknowledged text whose
 *  CR did not land is `ambiguous` and never a rejection, because those bytes
 *  are already on the PTY and the agent may hold an unsubmitted draft. Never
 *  throws, so the caller's proven-zero-write rejections stay honest. */
export async function submitAgentPrompt(
  channelId: number,
  text: Uint8Array,
  budgetAdmitsSubmit: () => boolean,
): Promise<WorkerInputResult> {
  const written = await writeKeeperBatch(channelId, text);
  if (written.kind === "unwritten") {
    return { status: "rejected", writtenBytes: 0, reason: written.reason };
  }
  if (written.kind === "uncertain") {
    return {
      status: "ambiguous",
      writtenBytes: written.writtenBytes,
      reason: written.reason,
    };
  }
  await Bun.sleep(PROMPT_SUBMIT_DELAY_MS);
  if (!budgetAdmitsSubmit()) {
    return {
      status: "ambiguous",
      writtenBytes: written.writtenBytes,
      reason: "agent prompt submit was not written",
    };
  }
  const submitted = await writeKeeperBatch(channelId, CR_BYTES);
  if (submitted.kind === "acked") {
    return {
      status: "accepted",
      writtenBytes: written.writtenBytes + submitted.writtenBytes,
    };
  }
  return {
    status: "ambiguous",
    writtenBytes: written.writtenBytes
      + (submitted.kind === "uncertain" ? submitted.writtenBytes : 0),
    reason: submitted.kind === "unwritten"
      ? "keeper did not submit the agent prompt"
      : submitted.reason,
  };
}

async function writeKeeperBatch(
  channelId: number,
  bytes: Uint8Array,
): Promise<KeeperWriteOutcome> {
  let command;
  try {
    command = getMultiplexedPool().beginInput(channelId, bytes);
  } catch {
    return { kind: "uncertain", writtenBytes: 0, reason: "keeper input admission failed" };
  }
  if (!command.admission.written) {
    return { kind: "unwritten", reason: "keeper did not admit the agent prompt" };
  }
  let result;
  try {
    result = await command.result;
  } catch {
    return { kind: "uncertain", writtenBytes: 0, reason: "keeper input result unavailable" };
  }
  if (result.kind === "ack") {
    return result.writtenBytes === bytes.byteLength
      ? { kind: "acked", writtenBytes: result.writtenBytes }
      : {
          kind: "uncertain",
          writtenBytes: boundedWrittenBytes(result.writtenBytes, bytes.byteLength),
          reason: "keeper acknowledged an incomplete input batch",
        };
  }
  if (result.kind === "reject") {
    return { kind: "unwritten", reason: "keeper rejected the agent prompt" };
  }
  return {
    kind: "uncertain",
    writtenBytes: boundedWrittenBytes(result.writtenBytes, bytes.byteLength),
    reason: "keeper agent prompt outcome is ambiguous",
  };
}

function boundedWrittenBytes(value: number | null, expectedBytes: number): number {
  return value !== null
    && Number.isSafeInteger(value)
    && value >= 0
    && value <= expectedBytes
    ? value
    : 0;
}

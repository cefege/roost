// One tunnel call: SessionsChatCommand → coord → worker → `omp --mode rpc`.
// Every omp command from the chat UI (prompt, abort, set_model,
// set_thinking_level, get_available_models) goes through here so the envelope
// unwrap + failure toast live in ONE place instead of being re-typed per call
// site.

import { coordClient } from "../../../connect.ts";
import { addToast } from "../../../lib/toastStore.ts";

/** Returns the command's `data` payload on success — `{}` when the command
 *  carries no payload (set_model, set_thinking_level, abort all answer with a
 *  bare `success:true`) — and null on any failure, after firing a toast. null
 *  therefore means FAILED, never "succeeded with nothing to say".
 *
 *  `quiet` suppresses the failure toast only — the return contract is
 *  unchanged. For speculative fetches the user never asked for (the welcome
 *  card's tip list), a dead child must degrade the view, not raise an error. */
export async function ompCommand(
  sessionId: string,
  cmd: Record<string, unknown>,
  label: string,
  quiet = false,
): Promise<unknown | null> {
  try {
    const res = await coordClient.sessionsChatCommand({ sessionId, commandJson: JSON.stringify(cmd) });
    const parsed: unknown = JSON.parse(res.responseJson || "{}");
    if (!parsed || typeof parsed !== "object" || !("success" in parsed) || parsed.success !== true) {
      const err = parsed && typeof parsed === "object" && "error" in parsed
        ? String(parsed.error)
        : `${label} rejected`;
      if (!quiet) addToast(`Chat: ${err}`, "err");
      return null;
    }
    return "data" in parsed ? parsed.data : {};
  } catch (e) {
    if (!quiet) addToast(`${label} failed: ${e instanceof Error ? e.message : String(e)}`, "err");
    return null;
  }
}

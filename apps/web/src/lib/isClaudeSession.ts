// Shared "is this a claude session" predicate. A session spawned as a
// plain shell can have claude launched inside it — `s.kind` stays "shell"
// but the claude TUI is detected via scraped claude_status or agent state.
// Sidebar (SessionRow) and TabBar MUST agree, so both call this one rule.
import { rootStore } from "../store/root.ts";
import type { Session } from "@roost/shared/wire";

export function isClaudeSession(s: Session): boolean {
  return s.kind === "claude"
    || s.agent?.kind === "claude"
    || (rootStore.claude_status[s.id] ?? "unknown") !== "unknown";
}

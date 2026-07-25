// Tip projection for the empty-pane welcome card. Lives beside ChatWelcome.tsx
// rather than inside it (same split as thinkingText.ts / renderPlan.ts): a pure
// decoder is directly testable, a Solid component is not.

/** Commands worth surfacing in an empty web pane, best first. omp's own
 *  description text is used verbatim; a name absent from this build is
 *  skipped, so the list degrades instead of lying. */
export const TIP_COMMANDS = ["model", "context", "compact", "plan", "todo", "usage"];
export const MAX_TIPS = 4;

export interface Tip {
  name: string;
  description: string;
}

/** Decode omp's get_available_commands payload
 *  (`{ commands: [{ name, description?, … }] }`) into the handful of tips we
 *  show. Guarded field by field like ModelMenu::parseModels — it crossed a JSON
 *  tunnel, and a malformed entry is dropped rather than rendered half-built. */
export function pickTips(data: unknown): Tip[] {
  if (!data || typeof data !== "object" || !("commands" in data)) return [];
  const raw = data.commands;
  if (!Array.isArray(raw)) return [];
  const byName = new Map<string, string>();
  for (const c of raw) {
    if (!c || typeof c !== "object") continue;
    const name = "name" in c && typeof c.name === "string" ? c.name : "";
    const description = "description" in c && typeof c.description === "string" ? c.description : "";
    if (!name || !description) continue;
    byName.set(name, description);
  }
  const out: Tip[] = [];
  for (const name of TIP_COMMANDS) {
    if (out.length >= MAX_TIPS) break;
    const description = byName.get(name);
    if (description) out.push({ name, description });
  }
  return out;
}

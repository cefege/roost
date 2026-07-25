// Render-parity harness (humanchrome, not Playwright — same pattern as
// /roost-smoke run.js). Proves the omp chat UI reaches PARITY with the raw
// transcript. This is the visual/render side the deterministic
// tests/ompChatCoverage.test.ts (routing) cannot see.
//
// PROCEDURE
//   1. bun apps/web/scripts/genChatFixture.ts   # refresh fixture from a live session
//   2. vite dev serving apps/web  (bun run --filter @roost/web dev)
//   3. Drive a humanchrome tab to /chat-render-real.html, inject IN_TAB_EXTRACT,
//      and pass its result plus the fixture JSON to checkParity.
//   4. PASS when misses === 0 AND cards.missing === 0.
//
// TWO oracles, because the pane has two rendering contracts:
//   - PROSE (message text, reasoning, tool-call args) must be reachable
//     VERBATIM. Fuzzy-robust: 30-char alphanumeric windows slide against the
//     normalized rendered text, so markdown transforms never cause false misses.
//   - TOOL RESULTS are rendered by omp's own <omp-tool-view> cards, which
//     SUMMARIZE by design (a write shows path + line delta, a read caps its
//     body at a dozen lines). Asserting their text verbatim would assert that
//     omp's renderers are broken. The contract is structural instead: every
//     call that produced a result has a card, named for its tool.
//
// Reasoning and tool bodies are collapsed by default — IN_TAB_EXTRACT clicks
// every `Thinking …` line and every card head first (real clicks), so a
// regression that leaves content unreadable when expanded is still caught.

/** Build the prose oracle: one content string per verbatim-rendered block. */
export function contentUnits(messages) {
  const units = [];
  for (const m of messages) {
    for (const b of m.blocks) {
      if (b.kind === "text" || b.kind === "thinking") units.push({ kind: b.kind, content: b.text });
      else if (b.kind === "toolCall") {
        let a = {};
        try { a = JSON.parse(b.argsJson || "{}"); } catch { /* ignore */ }
        units.push({ kind: b.kind, content: String(a.path ?? a.content ?? a.input ?? a.command ?? a.pattern ?? a.query ?? a.op ?? b.name) });
      }
      // toolResult → the card oracle below. image → asserted separately
      // (an <img> or a visible placeholder), not by text.
    }
  }
  return units.filter((u) => u.content && u.content.length >= 12);
}

/** Tool names the thread must paint a card for — one per callId that a call or
 *  a result introduced. MCP names are mapped the way ToolCard prints them
 *  (omp's parseMCPToolName: `mcp__server_tool` → `server/tool`). */
export function expectedToolCards(messages) {
  const byCallId = new Map();
  for (const m of messages) {
    for (const b of m.blocks) {
      if (b.kind === "toolCall" || b.kind === "toolResult") byCallId.set(b.callId, displayToolName(b.name));
    }
  }
  return [...byCallId.values()];
}

function displayToolName(name) {
  if (!name.startsWith("mcp__")) return name;
  const rest = name.slice(5);
  const i = rest.indexOf("_");
  return i === -1 ? name : `${rest.slice(0, i)}/${rest.slice(i + 1)}`;
}

/** Is `content` reachable somewhere in `rendered`? Slides 30-char windows. */
export function contentPresent(rendered, content) {
  const norm = (s) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
  const R = norm(rendered);
  const n = norm(content);
  const W = 30;
  if (n.length <= W) return R.includes(n);
  for (let off = 0; off + W <= n.length; off += 15) if (R.includes(n.slice(off, off + W))) return true;
  return false;
}

/** Compute parity. `extract` is IN_TAB_EXTRACT's result: {text, cardNames}. */
export function checkParity(messages, extract) {
  const units = contentUnits(messages);
  const misses = units.filter((u) => !contentPresent(extract.text, u.content));

  // Multiset difference: two `read` calls need two `read` cards.
  const painted = [...extract.cardNames];
  const missingCards = [];
  for (const name of expectedToolCards(messages)) {
    const at = painted.indexOf(name);
    if (at === -1) missingCards.push(name);
    else painted.splice(at, 1);
  }

  return {
    blocksChecked: units.length,
    misses: misses.length,
    coveragePct: units.length ? (((units.length - misses.length) / units.length) * 100).toFixed(1) : "100.0",
    examples: misses.slice(0, 10).map((u) => ({ kind: u.kind, preview: u.content.slice(0, 80) })),
    cards: { expected: expectedToolCards(messages).length, painted: extract.cardNames.length, missing: missingCards },
  };
}

/** In-tab driver (inject via humanchrome). Expands every disclosure, then
 *  returns the thread text plus the name of every painted tool card. */
export const IN_TAB_EXTRACT = `(() => {
  document.querySelectorAll('.tr-think-collapsed').forEach((b) => b.click());
  document.querySelectorAll('omp-tool-view .tv-head[aria-expanded="false"]').forEach((b) => b.click());
  return new Promise((r) => setTimeout(() => r({
    text: document.querySelector('.omp-chat__thread').innerText,
    cardNames: [...document.querySelectorAll('omp-tool-view .tv-name')].map((e) => e.textContent),
  }), 800));
})()`;

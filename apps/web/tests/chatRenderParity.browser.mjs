// Render-parity harness (humanchrome, not Playwright — same pattern as
// /roost-smoke run.js). Proves the omp chat UI reaches PARITY with the raw
// transcript: every content unit the client receives (message text, reasoning,
// tool-call args, tool-result text — across ALL result blocks) is reachable in
// the rendered DOM. This is the visual/render side the deterministic
// tests/ompChatCoverage.test.ts (routing) cannot see.
//
// PROCEDURE
//   1. bun apps/web/scripts/genChatFixture.ts   # refresh fixture from a live session
//   2. vite dev serving apps/web  (bun run --filter @roost/web dev)
//   3. Drive a humanchrome tab to /chat-render-real.html and inject the body of
//      runParity() below (read the fixture JSON on the Node side, pass it in).
//   4. PASS when coveragePct === "100.0" and misses === 0.
//
// The comparison is fuzzy-robust: it slides 30-char alphanumeric windows of each
// unit against the normalized rendered text, so markdown transforms, tag names,
// and field ordering never cause false misses. Reasoning lives in collapsed
// <details> — the harness opens every thinking summary first (a real click), so
// a regression that leaves thinking unreadable when expanded is caught.

/** Build the parity oracle: one content string per renderable block. */
export function contentUnits(messages) {
  const units = [];
  for (const m of messages) {
    for (const b of m.blocks) {
      if (b.kind === "text" || b.kind === "thinking" || b.kind === "toolResult") units.push({ kind: b.kind, content: b.text });
      else if (b.kind === "toolCall") {
        let a = {};
        try { a = JSON.parse(b.argsJson || "{}"); } catch { /* ignore */ }
        units.push({ kind: b.kind, content: String(a.path ?? a.content ?? a.input ?? a.command ?? a.pattern ?? a.query ?? a.op ?? b.name) });
      }
      // image → asserted separately (an <img> or a visible placeholder), not by text.
    }
  }
  return units.filter((u) => u.content && u.content.length >= 12);
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

/** Compute parity given the messages and the rendered thread innerText. */
export function checkParity(messages, renderedText) {
  const units = contentUnits(messages);
  const misses = units.filter((u) => !contentPresent(renderedText, u.content));
  return {
    blocksChecked: units.length,
    misses: misses.length,
    coveragePct: units.length ? (((units.length - misses.length) / units.length) * 100).toFixed(1) : "100.0",
    examples: misses.slice(0, 10).map((u) => ({ kind: u.kind, preview: u.content.slice(0, 80) })),
  };
}

/** In-tab driver (inject via humanchrome). Opens all reasoning, returns thread text. */
export const IN_TAB_EXTRACT = `(() => {
  document.querySelectorAll('.omp-thinking summary').forEach((s) => s.click());
  return new Promise((r) => setTimeout(() => r(document.querySelector('.omp-chat__thread').innerText), 500));
})()`;

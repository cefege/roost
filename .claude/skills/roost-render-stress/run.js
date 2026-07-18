// Render-correctness STRESS loop (in-page). Hammers the REAL resize path —
// deck style.width/height → ResizeObserver → viewport claim → PTY resize →
// scrollback re-derive — in a loop and asserts the painted grid never loses,
// duplicates, or mis-orders history. Runs hundreds of iterations with NO
// per-step MCP round-trip, logging every iteration to the console.
//
// Why this exists: the wire/data-* smoke passed green while the painted grid
// was corrupt (rows duplicated / scroll jumped to top / history lost on
// tab-switch). Those defects are ONLY visible in what renders. This harness
// asserts what renders, under the exact perturbation (viewport size wobble)
// that drives the corruption per project_terminal_history_corruption_viewport_slaved_pty.
//
// Driven by the roost-render-stress skill, which sets, before injecting:
//   window.__stressSid    = session id (already filled with `${prefix}<N>` history)
//   window.__stressIter   = iteration count (default 80)
//   window.__stressSettle = ms to wait per resize for settle (default 350)
//   window.__stressPrefix = marker prefix (default "ROOSTLINE-")
// Run it once per mode (byte + cell) and once with a 2nd passive viewer tab.

return await (async () => {
  const sm = window.__smoke;
  if (!sm) return { err: "no __smoke — enable localStorage.roostSmoke=1 + reload" };
  const sid = window.__stressSid;
  const prefix = window.__stressPrefix || "ROOSTLINE-";
  const ITER = window.__stressIter || 80;
  const SETTLE = window.__stressSettle || 350;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  const deck = document.querySelector('[data-testid="terminal-deck"]');
  if (!deck) return { err: "no terminal-deck" };
  const probe0 = sm.renderProbe(sid);
  if (!probe0.found) return { err: "no pane for session", sid };

  const baseline = sm.markerScan(sid, prefix);
  if (baseline.max === 0) {
    return { err: "no markers rendered — generate history first", baseline, mode: probe0.mode };
  }

  const restore = deck.style.cssText;
  const r0 = deck.getBoundingClientRect();
  const W0 = r0.width, H0 = r0.height;

  // perturbation matrix: [label, dW, dH]. Covers width-only, height-only,
  // both, large (out-of-band) and tiny (in-band wobble) — the exact mix the
  // user called out (swap sizes, smaller/larger, width vs height, one by one).
  const OPS = [
    ["w-shrink", -200, 0], ["w-grow", 200, 0],
    ["h-shrink", 0, -180], ["h-grow", 0, 180],
    ["both-shrink", -200, -180], ["both-grow", 200, 180],
    ["w-tiny", -48, 0], ["h-tiny", 0, -48],
  ];

  const fails = [];
  const log = [];
  for (let i = 0; i < ITER; i++) {
    const [label, dW, dH] = OPS[i % OPS.length];
    deck.style.width = Math.max(220, W0 + dW) + "px";
    deck.style.height = Math.max(180, H0 + dH) + "px";
    deck.style.maxHeight = Math.max(180, H0 + dH) + "px";
    await sleep(SETTLE);
    // Half the time wobble back to base size, the way chrome wobble does.
    if (i % 2 === 1) {
      deck.style.width = W0 + "px";
      deck.style.height = H0 + "px";
      deck.style.maxHeight = H0 + "px";
      await sleep(SETTLE);
    }
    const scan = sm.markerScan(sid, prefix);
    const probe = sm.renderProbe(sid);
    const f = [];
    if (scan.duplicated.length) f.push("DUP=" + scan.duplicated.slice(0, 6).join(","));
    if (scan.max !== baseline.max) f.push("DEPTH max=" + scan.max + " base=" + baseline.max);
    if (scan.missing > baseline.missing) f.push("LOSS missing=" + scan.missing + " base=" + baseline.missing);
    if (scan.outOfOrder > 0) f.push("MANGLE inversions=" + scan.outOfOrder + " firstAt=" + scan.firstInversion);
    const rec = { i, op: label, dup: scan.duplicated.length, max: scan.max,
      missing: scan.missing, mangle: scan.outOfOrder, rows: probe.rowCount,
      fromBottom: probe.fromBottom, fails: f };
    log.push(rec);
    if (f.length) { fails.push(rec); console.warn("[stress FAIL]", JSON.stringify(rec)); }
    else console.log("[stress ok]", JSON.stringify(rec));
  }

  // Restore + assert recovery: back at the original size, depth must equal
  // baseline (a permanent drift means the resize path bled history forever).
  deck.style.cssText = restore;
  await sleep(SETTLE * 2);
  const final = sm.markerScan(sid, prefix);
  const restoredOk = final.max === baseline.max && final.duplicated.length === 0;

  return {
    sessionId: sid, mode: probe0.mode, iterations: ITER,
    baseline: { max: baseline.max, missing: baseline.missing, unique: baseline.unique },
    failCount: fails.length, fails: fails.slice(0, 25),
    final: { max: final.max, missing: final.missing, dup: final.duplicated.length },
    restoredOk,
    verdict: fails.length === 0 && restoredOk ? "PASS" : "FAIL",
  };
})();

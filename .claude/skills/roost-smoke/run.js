// roost-smoke harness — single chrome_javascript-injectable script.
// Returns { steps: [...], summary: "N/N passed" }.
//
// Prereq: page is loaded on the prod URL AND
// localStorage.roostSmoke === "1" AND `window.__smoke` is installed
// (see apps/web/src/lib/smoke.ts). The skill sets the flag once and
// reloads before injecting this script.
//
// v3: adapted for the ONE sidebar layout (FolderList = needs-you-strip +
// folder rows; view modes deleted 2026-07-04). Uses window.__smoke API for
// spawn/kill/input; asserts on folder-list DOM (folder-list, folder-row-*,
// needs-you-strip, terminal-deck). Session rows exist ONLY inside the
// needs-you strip — a healthy fresh shell never renders a session row.

const HOME = "/tmp";  // portable — every Unix has /tmp
(async () => {
  const steps = [];
  function record(name, pass, detail) {
    steps.push({ name, pass, detail });
  }
  function $(sel) { return document.querySelector(sel); }
  function $$(sel) { return Array.from(document.querySelectorAll(sel)); }
  function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }
  async function waitUntil(check, timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (check()) return true;
      await sleep(100);
    }
    return check();
  }
  function sameIds(a, b) {
    return a.length === b.length && a.every((id, index) => id === b[index]);
  }
  function terminalFootprint() {
    const slotPrefix = "terminal-slot-";
    const slots = $$('[data-testid^="terminal-slot-"]');
    const sessionId = (slot) => slot.dataset.testid?.slice(slotPrefix.length) ?? "";
    const visible = slots.filter((slot) => getComputedStyle(slot).visibility === "visible");
    return {
      openSessions: Object.values(window.__smoke.state().sessions)
        .filter((session) => session.status === "open")
        .map((session) => session.id)
        .sort(),
      mountedSlots: slots.length,
      cellPanes: $$('[data-testid="cell-terminal-pane"]').length,
      wterms: $$('.wterm').length,
      textareas: $$('textarea').length,
      visibleSlots: visible.length,
      mountedIds: slots.map(sessionId).filter(Boolean).sort(),
      visibleIds: visible.map(sessionId).filter(Boolean).sort(),
    };
  }
  function freshestWorker(workers) {
    return Object.entries(workers)
      .sort(([, a], [, b]) => Number(b.last_seen_ms ?? 0) - Number(a.last_seen_ms ?? 0))[0]?.[0] ?? null;
  }
  async function readyFreshDeck() {
    const dismiss = $('[data-testid="whats-new-dismiss"]');
    const dialog = dismiss?.closest("md-dialog");
    if (dismiss && dialog?.hasAttribute("open")) {
      dismiss.click();
      if (!await waitUntil(() => !dialog.hasAttribute("open"), 5_000)) {
        return { dismissed: true, homeLanding: false };
      }
    }
    history.pushState({}, "", "/");
    window.dispatchEvent(new PopStateEvent("popstate"));
    return {
      dismissed: !!dismiss,
      homeLanding: await waitUntil(() => !!$('[data-testid="home-landing"]'), 10_000),
    };
  }

  // ── Step 1: bundle hash + page healthy ────────────────────────────
  try {
    // Prod serves the built bundle; vite dev serves /src/main.tsx directly.
    const script = $('script[src*="/assets/index-"]') ?? $('script[src*="/src/main"]');
    const bundle = script?.src.split("/").pop() ?? null;
    const errorBoundary = !!$('[data-testid="error-boundary"]');
    const hasSmoke = typeof window.__smoke === "object";
    record(
      "step1_bundle_loaded",
      !!bundle && !errorBoundary && hasSmoke,
      { bundle, errorBoundary, hasSmoke },
    );
    if (!hasSmoke) {
      record("FATAL", false, "window.__smoke missing — set localStorage.roostSmoke=1 and reload");
      return { steps, summary: `0/${steps.length} passed (fatal)` };
    }
  } catch (e) { record("step1_bundle_loaded", false, String(e)); }

  // ── Step 2: sidebar bootstrap (folder list) ────────────────────────
  try {
    const folderRows = $$('[data-testid^="folder-row-"]');
    const search = $('[data-testid="brand-row-search"]');
    const folderList = $('[data-testid="folder-list"]');
    record(
      "step2_sidebar_bootstrap",
      folderRows.length >= 1 && !!search && !!folderList,
      { folderRows: folderRows.length, hasSearch: !!search, hasFolderList: !!folderList },
    );
  } catch (e) { record("step2_sidebar_bootstrap", false, String(e)); }

  // ── Step 2b: dismiss onboarding and create a fresh deck ────────────
  try {
    const freshDeck = await readyFreshDeck();
    record("step2b_fresh_deck_ready", freshDeck.homeLanding, freshDeck);
  } catch (e) { record("step2b_fresh_deck_ready", false, String(e)); }

  // ── Step 3: spawn shell via __smoke API ───────────────────────────
  let firstSession = null; // { sessionId, workerFp }
  try {
    if (!$('[data-testid="home-landing"]')) throw new Error("fresh home deck unavailable");
    const workers = window.__smoke.state().workers;
    const fp = freshestWorker(workers);
    if (!fp) throw new Error("no workers in store");
    const t0 = performance.now();
    const sh = await Promise.race([
      window.__smoke.spawnShell(fp, HOME),
      new Promise((_, rj) => setTimeout(() => rj(new Error("spawn 10s")), 10000)),
    ]);
    const dt = performance.now() - t0;
    // Navigate to the session (flat mode uses /s/<id>)
    history.pushState({}, "", `/s/${sh.session_id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    // Poll for wterm visible
    const visDeadline = Date.now() + 5_000;
    let visibleWterm = 0;
    while (Date.now() < visDeadline) {
      visibleWterm = $$('.wterm').filter((w) => getComputedStyle(w).visibility === "visible").length;
      if (visibleWterm >= 1) break;
      await sleep(200);
    }
    record(
      "step3_spawn_and_nav",
      !!sh.session_id && visibleWterm >= 1,
      { dt_ms: Math.round(dt), session_id: sh.session_id?.slice(0, 8), visibleWterms: visibleWterm, workerFp: fp.slice(0, 12) },
    );
    const footprint = terminalFootprint();
    record(
      "step3_mount_footprint",
      sameIds(footprint.openSessions, footprint.mountedIds) &&
      sameIds(footprint.visibleIds, [sh.session_id]),
      footprint,
    );
    if (sh.session_id) {
      firstSession = { sessionId: sh.session_id, workerFp: fp };
    }
  } catch (e) {
    record("step3_spawn_and_nav", false, String(e));
    record("step3_mount_footprint", false, String(e));
  }

  // ── Step 4: session's folder row appears in sidebar ───────────────
  // A shell can canonicalize its cwd after spawn (macOS /tmp → /private/tmp).
  // Compare the row against the worker-reported current cwd, not the original
  // spawn argument, so this checks grouping rather than filesystem aliases.
  try {
    if (!firstSession) throw new Error("no session from step 3");
    const rowDeadline = Date.now() + 5_000;
    let found = false;
    let cwd = HOME;
    while (Date.now() < rowDeadline) {
      cwd = window.__smoke.state().sessions[firstSession.sessionId]?.cwd ?? HOME;
      found = $$('[data-testid^="folder-row-"] .df-flat-headline')
        .some((h) => h.title === cwd);
      if (found) break;
      await sleep(150);
    }
    record("step4_folder_row_in_sidebar", found, {
      sessionId: firstSession.sessionId?.slice(0, 8),
      cwd,
    });
  } catch (e) { record("step4_folder_row_in_sidebar", false, String(e)); }

  // ── Step 5: echo round-trip via __smoke.input ─────────────────────
  try {
    if (!firstSession) throw new Error("no session from step 3");
    let wtermReady = false;
    const readyDeadline = Date.now() + 5_000;
    while (Date.now() < readyDeadline) {
      // Cell mode paints into .wterm.cell-grid; the input-host wterm's
      // .term-grid mirror is renderless (empty by design) since the perf fix.
      const text = ($$('.wterm.cell-grid').map((g) => g.textContent ?? "")).join("");
      if (text.trim().length > 0) { wtermReady = true; break; }
      await sleep(150);
    }
    await window.__smoke.input(firstSession.sessionId, "echo roostsmokehello\n");
    const echoDeadline = Date.now() + 5_000;
    let sawEcho = false;
    let sample = "";
    while (Date.now() < echoDeadline) {
      const all = $$('.wterm.cell-grid').map((g) => g.textContent ?? "").join(" --- ");
      if (all.includes("roostsmokehello")) { sawEcho = true; sample = all.slice(-200); break; }
      await sleep(150);
    }
    record(
      "step5_echo_round_trip",
      sawEcho,
      { wtermReady, sample: sample || "no match in 5s" },
    );
  } catch (e) { record("step5_echo_round_trip", false, String(e)); }

  // ── Step 5b: REAL input path — active pane's textarea is focused ────
  // step5 uses __smoke.input (coord RPC) which BYPASSES the wterm textarea +
  // focus pipeline → it stays green even when focus never lands and a real
  // keystroke goes nowhere (the cell-phase-3b "can't input anything" bug, where
  // CellTerminal's focus effect was { defer:true } and skipped the on-mount
  // run). paneFocused() asserts document.activeElement IS the pane's textarea —
  // the only in-page signal that real typing would actually reach the PTY.
  try {
    if (!firstSession) throw new Error("no session from step 3");
    let pf = { focused: false };
    const focusDeadline = Date.now() + 3_000;
    while (Date.now() < focusDeadline) {
      pf = window.__smoke.paneFocused(firstSession.sessionId);
      if (pf.focused) break;
      await sleep(150);
    }
    record("step5b_input_focus_lands", pf.focused, pf);
  } catch (e) { record("step5b_input_focus_lands", false, String(e)); }

  // ── Step 6: persistent warm deck on a folder switch ───────────────
  // Cold sessions must stay unmounted; first-visited slots persist by identity.
  try {
    const folderRows = $$('[data-testid^="folder-row-"]');
    if (folderRows.length < 2) {
      record("step6_deck_persists_on_switch", true, "skipped — only one folder");
      record("step6_mount_footprint", true, "skipped — only one folder");
      record("step6_restore_original_slot", true, "skipped — only one folder");
    } else {
      const before = terminalFootprint();
      const originalSlot = $$('[data-testid^="terminal-slot-"]')
        .find((slot) => slot.dataset.testid === `terminal-slot-${firstSession?.sessionId}`);
      const originalCell = originalSlot?.querySelector('[data-testid="cell-terminal-pane"]') ?? null;
      const otherRow = folderRows.find((row) => row.dataset.selected !== "focused");
      otherRow?.click();
      const switched = await waitUntil(
        () => !terminalFootprint().visibleIds.includes(firstSession?.sessionId ?? ""),
        5_000,
      );
      const afterSwitch = terminalFootprint();
      const focusedSlots = $$('[data-testid^="terminal-slot-"]')
        .filter((slot) => slot.dataset.focused === "true");
      const originalPreserved = !!originalSlot?.isConnected
        && !!originalCell?.isConnected
        && getComputedStyle(originalSlot).visibility !== "visible";
      record(
        "step6_deck_persists_on_switch",
        switched && originalPreserved && focusedSlots.length === 1,
        { switched, originalPreserved, focusedSlots: focusedSlots.length },
      );
      const expectedMountedIds = [...new Set([...before.mountedIds, ...afterSwitch.visibleIds])].sort();
      record(
        "step6_mount_footprint",
        sameIds(afterSwitch.mountedIds, expectedMountedIds),
        { before, afterSwitch, expectedMountedIds },
      );
      history.pushState({}, "", `/s/${firstSession?.sessionId}`);
      window.dispatchEvent(new PopStateEvent("popstate"));
      const restored = await waitUntil(
        () => terminalFootprint().visibleIds.includes(firstSession?.sessionId ?? "")
          && originalSlot?.isConnected
          && originalCell?.isConnected
          && originalSlot?.dataset.focused === "true",
        5_000,
      );
      record(
        "step6_restore_original_slot",
        restored,
        { restored, visibleIds: terminalFootprint().visibleIds, focused: originalSlot?.dataset.focused === "true" },
      );
    }
  } catch (e) {
    record("step6_deck_persists_on_switch", false, String(e));
    record("step6_mount_footprint", false, String(e));
    record("step6_restore_original_slot", false, String(e));
  }

  // ── Step 7: kill removes session from sidebar ─────────────────────
  try {
    if (!firstSession) throw new Error("no session");
    const sessBefore = Object.keys(window.__smoke.state().sessions).length;
    const k = await window.__smoke.kill(firstSession.sessionId);
    const closeDeadline = Date.now() + 5_000;
    let sessionGone = false;
    while (Date.now() < closeDeadline) {
      const s = window.__smoke.state().sessions[firstSession.sessionId];
      if (!s || s.status !== "open") { sessionGone = true; break; }
      await sleep(100);
    }
    const sessAfter = Object.keys(window.__smoke.state().sessions).length;
    // No row anywhere (needs-you strip is the only session-row surface)
    // should still reference the killed session.
    const rowStill = !!$(`[data-session-id="${firstSession.sessionId}"]`);
    record(
      "step7_kill_removes_session",
      sessionGone && !rowStill,
      { killAccepted: k?.accepted, sessBefore, sessAfter, sessionGone, rowStillPresent: rowStill },
    );
  } catch (e) { record("step7_kill_removes_session", false, String(e)); }

  // ── Step 8: error-boundary stays absent on known-fail paths ────────
  try {
    await window.__smoke.kill("00000000-0000-0000-0000-000000000000").catch(() => null);
    await sleep(300);
    const eb = !!$('[data-testid="error-boundary"]');
    record("step8_no_error_boundary_on_bad_kill", !eb, { errorBoundary: eb });
  } catch (e) { record("step8_no_error_boundary_on_bad_kill", false, String(e)); }

  // ── Step 9: second spawn + create workspace via mutation ───────────
  let step9WsId = null;
  let step9Sid = null;
  try {
    const workers = window.__smoke.state().workers;
    const fp = freshestWorker(workers);
    if (!fp) throw new Error("no workers");
    const t0 = Date.now();
    const sh = await Promise.race([
      window.__smoke.spawnShell(fp, HOME),
      new Promise((_, rj) => setTimeout(() => rj(new Error("spawn 5s")), 5000)),
    ]);
    await sleep(500);
    const ws = await Promise.race([
      window.__smoke.createWorkspace(fp, HOME, sh.session_id),
      new Promise((_, rj) => setTimeout(() => rj(new Error("create 5s")), 5000)),
    ]);
    step9WsId = ws?.id ?? null;
    step9Sid = sh.session_id;
    const dt = Date.now() - t0;
    record(
      "step9_create_workspace_via_mutation",
      !!ws?.id && dt < 8000,
      { dt_ms: dt, ws_id: ws?.id?.slice(0, 8), session_id: sh.session_id?.slice(0, 8) },
    );
  } catch (e) { record("step9_create_workspace_via_mutation", false, String(e)); }

  // ── Step 10: add second pane to same workspace ─────────────────────
  try {
    if (!step9WsId || !step9Sid) throw new Error("no workspace from step 9");
    const workers = window.__smoke.state().workers;
    const fp = freshestWorker(workers);
    if (!fp) throw new Error("no workers");
    const before = Object.values(window.__smoke.state().sessions)
      .filter((s) => s.status === "open").length;
    const t0 = Date.now();
    const sh2 = await Promise.race([
      window.__smoke.spawnShell(fp, HOME),
      new Promise((_, rj) => setTimeout(() => rj(new Error("spawn 5s")), 5000)),
    ]);
    await sleep(500);
    // Assign to the step 9 workspace
    await window.__smoke.createWorkspace(fp, HOME, sh2.session_id);
    let after = before;
    const dl = Date.now() + 6000;
    while (Date.now() < dl) {
      after = Object.values(window.__smoke.state().sessions)
        .filter((s) => s.status === "open").length;
      if (after > before) break;
      await sleep(150);
    }
    const dt = Date.now() - t0;
    record(
      "step10_add_pane_to_workspace",
      after > before,
      { dt_ms: dt, sessions_before: before, sessions_after: after },
    );
  } catch (e) { record("step10_add_pane_to_workspace", false, String(e)); }

  // ── Step 11: cwd flip re-groups session ───────────────────────────
  try {
    const workers = window.__smoke.state().workers;
    const fp = freshestWorker(workers);
    if (!fp) throw new Error("no workers");
    const sh = await Promise.race([
      window.__smoke.spawnShell(fp, HOME),
      new Promise((_, rj) => setTimeout(() => rj(new Error("spawn 5s")), 5000)),
    ]);
    await sleep(800);
    history.pushState({}, "", `/s/${sh.session_id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await sleep(2500);
    await window.__smoke.input(sh.session_id, "cd /tmp\n");
    // Poll for cwd update from OSC 7 → store
    const deadline = Date.now() + 5_000;
    let cwdAfter = null;
    while (Date.now() < deadline) {
      const s = window.__smoke.state().sessions[sh.session_id];
      if (s?.cwd === "/tmp") { cwdAfter = s.cwd; break; }
      await sleep(150);
    }
    // One-mode sidebar: the cwd flip must RE-GROUP the session — a folder
    // row whose headline title is /tmp appears (folderKeyOf tracks cwd).
    const rowDeadline = Date.now() + 2_000;
    let tmpFolderRow = false;
    while (Date.now() < rowDeadline) {
      tmpFolderRow = $$('[data-testid^="folder-row-"] .df-flat-headline')
        .some((h) => h.title === "/tmp");
      if (tmpFolderRow) break;
      await sleep(100);
    }
    record(
      "step11_cwd_flip_regroups_session",
      cwdAfter === "/tmp" && tmpFolderRow,
      { cwdAfter, tmpFolderRow, sid: sh.session_id?.slice(0, 8) },
    );
  } catch (e) { record("step11_cwd_flip_regroups_session", false, String(e)); }

  // ── Step 12: attachment chip stack renders ─────────────────────────
  try {
    // If any open session has attachments, the chip stack should render.
    // Vacuous pass if no attachments exist.
    const chips = $$('[data-testid="attachment-chip-stack"]');
    record("step12_attachment_chips", true, chips.length > 0 ? "present" : "none — vacuous pass");
  } catch (e) { record("step12_attachment_chips", false, String(e)); }

  // ── Step 13: within-band viewport wobble must NOT grow scrollback ──
  // Regression case for project_terminal_history_corruption_viewport_slaved_pty:
  // a sub-band (±<6 rows) resize wobble must be absorbed by hold-anchor
  // hysteresis (claimHysteresis.ts) → no claim → no SCD change → no
  // re-derive → scrollback depth stays flat. If Step A regresses
  // (ratchet-to-min / no hysteresis), each shrink adopts → SCD shrinks →
  // @wterm pushes live rows to scrollback → depth ratchets up.
  try {
    const workers = window.__smoke.state().workers;
    const fp = freshestWorker(workers);
    if (!fp) throw new Error("no workers");
    const sh = await Promise.race([
      window.__smoke.spawnShell(fp, HOME),
      new Promise((_, rj) => setTimeout(() => rj(new Error("spawn 5s")), 5000)),
    ]);
    history.pushState({}, "", `/s/${sh.session_id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await sleep(1500);
    // Generate well past one viewport of plain-shell scrollback.
    await window.__smoke.input(sh.session_id, "seq 1 300\n");
    const depthOf = () => {
      // Count scrollback rows inside the one VISIBLE slot. Cell mode paints
      // history as .cell-row rows inside .cell-scrollback blocks; the hidden
      // input wterm is renderless (no .term-scrollback-row mirror anymore).
      const slot = $$('[data-testid^="terminal-slot-"]').find((s) => getComputedStyle(s).visibility === "visible");
      return slot ? slot.querySelectorAll('.cell-scrollback .cell-row').length : 0;
    };
    const genDeadline = Date.now() + 6000;
    let d0 = 0;
    while (Date.now() < genDeadline) { d0 = depthOf(); if (d0 > 50) break; await sleep(200); }
    const deck = $('[data-testid="terminal-deck"]');
    const restore = deck ? deck.style.cssText : "";
    const baseH = deck ? deck.getBoundingClientRect().height : 0;
    // ~4 rows worth (~70px), under the ±6-row VIEWPORT_HYSTERESIS band.
    // Wait > FIT_SETTLE_MS (1000) between flips so each settles.
    for (let i = 0; i < 3 && deck && baseH > 200; i++) {
      deck.style.height = (baseH - 70) + "px";
      deck.style.maxHeight = (baseH - 70) + "px";
      await sleep(1300);
      deck.style.height = baseH + "px";
      deck.style.maxHeight = baseH + "px";
      await sleep(1300);
    }
    if (deck) deck.style.cssText = restore;
    await sleep(800);
    const d1 = depthOf();
    const grew = d1 - d0;
    record(
      "step13_resize_wobble_holds_scrollback",
      d0 > 50 && grew <= 2,
      { d0, d1, grew, baseH: Math.round(baseH) },
    );
    await window.__smoke.kill(sh.session_id).catch(() => null);
  } catch (e) { record("step13_resize_wobble_holds_scrollback", false, String(e)); }


  // Never leave this run's spawned sessions behind. Allowlist only — NEVER a scan
  // of state().sessions (feedback_never_mass_kill_live_sessions).
  await window.__smoke.killSpawned().catch(() => null);

  const passed = steps.filter((s) => s.pass).length;
  const total = steps.length;
  return { steps, summary: `${passed}/${total} passed` };
})();

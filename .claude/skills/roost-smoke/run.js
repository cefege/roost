// roost-smoke harness — single chrome_javascript-injectable script.
// Returns { steps: [{ name, pass, detail }], summary: "N/12 passed" }.
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
      return { steps, summary: "0/13 passed (fatal)" };
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

  // ── Step 3: spawn shell via __smoke API ───────────────────────────
  let firstSession = null; // { sessionId, workerFp }
  try {
    const workers = window.__smoke.state().workers;
    const fps = Object.keys(workers);
    if (!fps.length) throw new Error("no workers in store");
    // Prefer a routable worker
    const fp = fps.find((f) => workers[f].reachable_addr) ?? fps[0];
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
    if (sh.session_id) {
      firstSession = { sessionId: sh.session_id, workerFp: fp };
    }
  } catch (e) { record("step3_spawn_and_nav", false, String(e)); }

  // ── Step 4: session's folder row appears in sidebar ───────────────
  // One-mode sidebar: a fresh shell shows as its (worker, folder) row —
  // headline title = spawn cwd — NOT a session row (those are strip-only).
  try {
    if (!firstSession) throw new Error("no session from step 3");
    const rowDeadline = Date.now() + 5_000;
    let found = false;
    while (Date.now() < rowDeadline) {
      found = $$('[data-testid^="folder-row-"] .df-flat-headline')
        .some((h) => h.title === HOME);
      if (found) break;
      await sleep(150);
    }
    record("step4_folder_row_in_sidebar", found, { sessionId: firstSession.sessionId?.slice(0, 8), cwd: HOME });
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

  // ── Step 6: deck persistence on switch (only with 2+ folders) ─────
  // Clicking a DIFFERENT folder row navigates to that folder's lead
  // session — guaranteed ≠ firstSession, so the deck must keep both panes.
  try {
    const folderRows = $$('[data-testid^="folder-row-"]');
    if (folderRows.length < 2) {
      record("step6_deck_persists_on_switch", true, "skipped — only one folder");
    } else {
      const deck = $('[data-testid="terminal-deck"]');
      const beforeChildren = deck?.children.length;
      const otherRow = folderRows.find((r) => r.dataset.selected !== "focused");
      otherRow?.click();
      await sleep(500);
      const afterChildren = deck?.children.length;
      // Cell mode: each active CellTerminal has TWO .wterm (display grid +
      // off-screen input host), so count visible SLOTS, not .wterm — exactly
      // one terminal-slot is visibility:visible at a time.
      const visibleNow = $$('[data-testid^="terminal-slot-"]').filter((s) => getComputedStyle(s).visibility === "visible").length;
      record(
        "step6_deck_persists_on_switch",
        beforeChildren === afterChildren && visibleNow === 1,
        { beforeChildren, afterChildren, visibleSlots: visibleNow },
      );
    }
  } catch (e) { record("step6_deck_persists_on_switch", false, String(e)); }

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
    const fp = Object.keys(workers).find((f) => workers[f].reachable_addr) ?? Object.keys(workers)[0];
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
    const fp = Object.keys(workers).find((f) => workers[f].reachable_addr) ?? Object.keys(workers)[0];
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
    const fp = Object.keys(workers).find((f) => workers[f].reachable_addr) ?? Object.keys(workers)[0];
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
    const fp = Object.keys(workers).find((f) => workers[f].reachable_addr) ?? Object.keys(workers)[0];
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

  // ── Step 14: quick chat spawns and the native pane renders ─────────
  // Proves the whole one-tap path: candidate pick → filesMkdir → spawnShell →
  // get_state probe (which boots `omp --mode rpc-ui`) → sidebar bucket →
  // chat-view routing. A quick chat is eligible BY CWD (ompChatEnabled →
  // isChatFolder), so no OSC title and no setChatView is involved — if the
  // pane does not render, routing or eligibility is genuinely broken.
  let chatSid = null;
  try {
    const qc = await window.__smoke.quickChat();
    chatSid = qc.session_id;
    history.pushState({}, "", `/s/${chatSid}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    const paneSel = `[data-testid="omp-chat-pane"][data-session-id="${chatSid}"]`;
    const t0 = performance.now();
    let hadPane = false, hadComposer = false;
    while (performance.now() - t0 < 15000) {
      const pane = $(paneSel);
      hadPane = !!pane;
      hadComposer = !!(pane && pane.querySelector('[data-testid="omp-chat-composer"]'));
      if (hadPane && hadComposer) break;
      await sleep(200);
    }
    record("step14_quick_chat_spawn_and_render", hadPane && hadComposer, {
      sessionId: chatSid, ms: Math.round(performance.now() - t0), hadPane, hadComposer,
    });
  } catch (e) { record("step14_quick_chat_spawn_and_render", false, String(e)); }

  // ── Step 15: a prompt streams back INCREMENTALLY ───────────────────
  // The bug this exists for: output appeared to arrive in one lump. Two
  // independent observations settle it — (a) the assistant text length grows
  // across several samples, so frames really are incremental; (b) the bubble's
  // DOM node is the SAME element throughout, so the keyed reconcile in
  // chatOmp.ts is holding and <For> is not tearing the row down and rebuilding
  // it (markdown re-parsed, selection lost) on every 60ms frame.
  try {
    if (!chatSid) throw new Error("step14 produced no session");
    const paneSel = `[data-testid="omp-chat-pane"][data-session-id="${chatSid}"]`;
    const res = await window.__smoke.chatPrompt(chatSid, "Write exactly four sentences about the ocean. No preamble.");
    if (!res.success) throw new Error(`worker rejected the prompt: ${res.error ?? "no error given"}`);

    const lens = [];       // distinct growing assistant text lengths
    let firstNode = null, sameNode = true, sawUser = false, sawStreaming = false, lastGrowth = 0;
    const t0 = performance.now();
    while (performance.now() - t0 < 90000) {
      const pane = $(paneSel);
      if (pane) {
        if (pane.querySelector('.tr-row--user [data-testid="omp-chat-msg"]')) sawUser = true;
        if (pane.querySelector('[data-testid="omp-chat-composer"][data-streaming="true"]')) sawStreaming = true;
        const bubbles = Array.from(pane.querySelectorAll('.tr-row--assistant [data-testid="omp-chat-msg"]'));
        const node = bubbles[bubbles.length - 1] ?? null;
        const len = node ? node.textContent.trim().length : 0;
        if (len > 0) {
          if (!firstNode) firstNode = node;
          else if (node !== firstNode) sameNode = false;
          if (lens[lens.length - 1] !== len) { lens.push(len); lastGrowth = performance.now(); }
        }
      }
      const st = window.__smoke.chatState(chatSid);
      // Only trust "the turn is over" AFTER the turn was observed to start —
      // right after chatPrompt resolves, `streaming` has not flipped true yet,
      // and exiting on that first tick would report a one-sample stream for a
      // perfectly healthy one.
      if (sawStreaming && lens.length > 0 && !st.streaming) break;
      // Fallback: a fast reply can finish between two 200ms samples and we may
      // never catch data-streaming="true". Settle on the text instead.
      if (lens.length >= 2 && performance.now() - lastGrowth > 3000) break;
      await sleep(200);
    }
    const st = window.__smoke.chatState(chatSid);
    const errorBoundary = !!$('[data-testid="error-boundary"]');
    // Two growing samples is the whole point: one sample means the reply only
    // ever appeared complete, which is the reported symptom, not a pass.
    // isConnected catches the remount directly — a rebuilt row leaves the node
    // we captured detached, which is exactly the pre-reconcile behavior.
    const stillMounted = !!firstNode && firstNode.isConnected;
    record("step15_chat_stream_round_trip",
      lens.length >= 2 && sameNode && stillMounted && sawUser && !errorBoundary, {
        growthSamples: lens.length, lens: lens.slice(0, 12), finalLen: lens[lens.length - 1] ?? 0,
        sameNode, stillMounted, sawUser, sawStreaming, errorBoundary,
        model: st.model, msgCount: st.msgCount, ms: Math.round(performance.now() - t0),
      });
  } catch (e) { record("step15_chat_stream_round_trip", false, String(e)); }

  // ── Step 16: an N-option decision renders and is answerable ────────
  // omp only registers its `ask` tool when it runs with a UI (hasUI =
  // isInteractive || mode === "rpc-ui"). Under the old `--mode rpc` child the
  // tool did not exist, so the agent could never offer a choice and this card
  // could never appear. Asserting the OPTION BUTTONS — not just the card —
  // also covers the worker's `options` mapping degrading to an empty array.
  try {
    if (!chatSid) throw new Error("step14 produced no session");
    const paneSel = `[data-testid="omp-chat-pane"][data-session-id="${chatSid}"]`;
    const ASK = "Use the ask tool right now to ask me one question: 'Pick a colour' with options Red, Green and Blue. Do nothing else.";
    let card = null, buttons = 0, pending = null, attempts = 0, panePresent = false, cardsAnywhere = 0;
    const t0 = performance.now();
    // Two attempts: whether the model reaches for `ask` on any single turn is
    // not a contract, and one re-ask is cheaper than a flaky gate. Still a FAIL
    // if it never asks — the capability is then unproven, and a green here
    // would be exactly the lie this step exists to prevent.
    while (attempts < 2 && !pending) {
      attempts++;
      const res = await window.__smoke.chatPrompt(chatSid, ASK);
      if (!res.success) throw new Error(`worker rejected the prompt: ${res.error ?? "no error given"}`);
      const deadline = performance.now() + 90000;
      let turnStarted = false;
      while (performance.now() < deadline) {
        const st = window.__smoke.chatState(chatSid);
        if (st.streaming) turnStarted = true;
        pending = st.approvals.find((a) => !a.resolved) ?? null;
        card = $(`${paneSel} [data-testid="omp-chat-approval"]`);
        // The card's controls are md-* custom elements, NOT <button> — count the
        // tagged option controls so an empty `options` array cannot pass.
        buttons = card ? card.querySelectorAll('[data-testid="omp-chat-approval-option"]').length : 0;
        panePresent = !!$(paneSel);
        cardsAnywhere = $$('[data-testid="omp-chat-approval"]').length;
        if (pending && card && buttons >= 3) break;
        // "Turn ended without asking" is only meaningful once the turn began —
        // `streaming` is still false for a moment after chatPrompt resolves.
        if (turnStarted && !pending && !st.streaming) break;
        await sleep(200);
      }
    }
    const options = pending ? pending.options : [];
    let answered = false;
    if (pending) {
      await window.__smoke.chatApprove(chatSid, pending.requestId, { value: options[0] ?? "Red" });
      const t1 = performance.now();
      while (performance.now() - t1 < 15000) {
        const a = window.__smoke.chatState(chatSid).approvals.find((x) => x.requestId === pending.requestId);
        if (a && a.resolved) { answered = true; break; }
        await sleep(200);
      }
    }
    record("step16_chat_decision_multi_option",
      !!pending && pending.method === "select" && options.length >= 3 && buttons >= 3 && answered, {
        method: pending ? pending.method : null, options, buttons, answered, attempts,
        panePresent, cardsAnywhere,
        reason: pending ? "" : "model never called the ask tool",
        ms: Math.round(performance.now() - t0),
      });
  } catch (e) { record("step16_chat_decision_multi_option", false, String(e)); }

  // ── Step 17: the chat toggle survives every omp run state ──────────
  // The 2c93d49a regression, driven with no omp process and no model cost.
  // omp encodes run state in the OSC title separator; the bug was call sites
  // anchored on `π >` / `π:`, the two forms a stock omp emits least.
  try {
    const workers = window.__smoke.state().workers;
    const fp = (firstSession && firstSession.workerFp)
      ?? Object.keys(workers).find((f) => workers[f].reachable_addr) ?? Object.keys(workers)[0];
    if (!fp) throw new Error("no workers");
    const sh = await window.__smoke.spawnShell(fp, HOME);
    history.pushState({}, "", `/s/${sh.session_id}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await sleep(1500);
    // Per-pane scoping (data-session-id, added by d691e13c): a bare
    // querySelector for the toggle is the wrong-pane trap this closes.
    const hasToggle = () => !!$(`[data-testid="cell-terminal-pane"][data-session-id="${sh.session_id}"] [data-testid="omp-chat-toggle"]`);
    const preTitle = hasToggle();
    const emit = async (title) => {
      await window.__smoke.input(sh.session_id, `printf '\\033]0;${title}\\007'\n`);
      await sleep(1200);
      return hasToggle();
    };
    const working = await emit("\u03c0 \u2839 smoke-working");    // spinner frame
    const attention = await emit("\u03c0 ! smoke-attention");      // blocked on the user
    const piOverwrite = await emit("\u03c0 - /tmp");               // pi's form; eligibility must LATCH
    record("step17_chat_toggle_survives_run_state",
      working && attention && piOverwrite,
      { preTitle, working, attention, piOverwrite, title: window.__smoke.state().sessions[sh.session_id]?.terminal_title ?? "" });
    await window.__smoke.kill(sh.session_id).catch(() => null);
  } catch (e) { record("step17_chat_toggle_survives_run_state", false, String(e)); }

  // Never leave this run's quick chat behind. Allowlist only — NEVER a scan of
  // state().sessions (feedback_never_mass_kill_live_sessions).
  await window.__smoke.killSpawned().catch(() => null);

  const passed = steps.filter((s) => s.pass).length;
  const total = steps.length;
  return { steps, summary: `${passed}/${total} passed` };
})();

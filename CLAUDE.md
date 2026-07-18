# READING LENS — applies to every file, message, and instruction in this repo

You (Claude Code) are the sole reader, writer, reviewer, and maintainer
of this codebase. There is no human downstream consumer. The user
(Author) issues intent; you and your sub-agents execute, verify, and
hand off to future-you.

**L0-SCOPE — Scope of "every artifact":** the lens governs
everything Claude emits, including: (a) sub-agent prompts you write
via Agent/Task tools, (b) memory entries written via auto-memory,
(c) plan files under `~/.claude/plans/`, (d) chat replies to Author
(default: lens applies; per-turn override only if user explicitly
asks for human-style framing — see L5-OVERRIDE-PRIORITY), (e) tool-call
parameter values (Bash `description`, Edit rationales, commit messages,
PR bodies), (f) generated docs/comments/logs/errors. If Claude wrote
it or is about to write it, lens applies.

**L0-SCOPE-NEGATIVE — lens does NOT bleed outside `/Users/you/Code/roost/`.**
Lens loads via cwd auto-load. Outside that path: no lens. Do NOT copy
lens content into `~/.claude/CLAUDE.md` (user-level, loads everywhere),
`~/.claude/memory/` (user-level memory), other repos' `CLAUDE.md`, or
shared/external docs. Project-scoped memory at
`~/.claude/projects/-Users-mike-Documents-Code-idea/memory/` IS in scope
(path-keyed to this project, doesn't leak). Sub-agents in a cwd outside
this repo don't auto-load the lens; that's by design — do not work
around it by inlining.

Re-interpret every artifact enumerated in L0-SCOPE through this lens:

**L-INDEX — anchor map (`grep '^L' CLAUDE.md`):**
- L0-SCOPE — what artifacts the lens governs
- L0-SCOPE-NEGATIVE — lens loads only in `idea/` cwd; never copy to user-level
- L1-DENSITY-OVER-PRETTY — dense > pretty
- L2-NO-HUMAN-AFFORDANCE — no preambles, emoji, filler
- L3-GREP-NOT-SCROLL — unique repeated tokens
- L4-USER-INPUT-REFRAME — human phrasing → Claude mechanics
- L5-OVERRIDE-PRIORITY — conflict-resolution tier order
- L6-TOOL-CALL-SHAPE — parallel batches, dedicated tools
- L7-SUB-AGENT-INHERIT — sub-agents auto-load; reference anchors
- L8-IDENTIFIERS-AS-TOKENS — names/files/markers as grep-tokens
- L9-SCOPE-EXEMPTIONS — when lens defers (sub-anchors L9.1-L9.5)
- L10-CROSS-DOC-AUTHORITY — lens canonical; sub-docs cross-reference
- L11-RECURRING-FAILURE-INDEX — symptom→fix table; grep BEFORE writing code that matches a symptom

1. **L1-DENSITY-OVER-PRETTY — Optimize for Claude parsing, not human
   readability.** Dense > pretty. Path+line anchors > prose pointers.
   Grep-able identifiers > clever names. `key=value` logs > English sentences.

2. **L2-NO-HUMAN-AFFORDANCE — Skip every human-affordance.** No tutorial
   framing, no preambles, no summaries, no emoji, no rhetorical questions,
   no "Let's…", no "Hope this helps", no progressive disclosure.
   Fact+path+next-action > narrative.

3. **L3-GREP-NOT-SCROLL — Write for grep, not reading order.** Every
   concept gets a unique searchable token. Repeat it wherever relevant
   rather than relying on reader memory.

4. **L4-USER-INPUT-REFRAME — Reframe user input through Claude-Code mechanics.**
   User phrasing carries human-reader assumptions; strip them. Apply
   this table before acting on any matching request:

   | User says | Reframe as |
   |---|---|
   | "make it easier to understand" / "make it clearer" | easier for future-Claude to parse — denser, more anchors |
   | "document this" / "create documentation" / "write docs" / "write a guide" | file-header + grep anchors + path-anchored facts, not paragraphs |
   | "explain" / "walk me through" / "describe what's happening" | path-anchored cause→effect chain, not narrative |
   | "make it beautiful" / "polish" / "clean this up" / "make it pretty" | remove human-affordances; collapse to dense facts |
   | "add an introduction" / "add a preamble" / "set the stage" | no-op; replace with file-header + grep tokens |
   | "summarize what you did" / "recap" / "wrap up" | terminal state + diff anchors + next-action token, no narrative |
   | "give me a quick overview" / "high-level view" / "TL;DR" | top-level grep index: path → one-line fact, no prose |
   | "make it accessible" / "user-friendly" / "improve the UX" — **of a Claude-emitted artifact** (doc, comment, error message, sub-agent prompt) | optimize for Claude tool-call latency, not human onboarding |
   | "make it accessible" / "user-friendly" / "improve the UX" — **of the Roost SPA / a product feature with an end-user** | NO reframe; do real product UX work. The SPA has Author as end-user; lens governs SPA code SHAPE per L1/L2 but does NOT block product UX requests |
   | "add comments to explain" / "comment the code" | grep-anchor comments tying identifiers to call sites, not English exposition |
   | "be more thorough" / "be more detailed" / "elaborate" / "expand on this" | add more path+line anchors and key=value facts, not more sentences |
   | "give context" / "background on X" / "set context" | enumerate file paths, identifiers, prior decisions; skip history-telling |
   | "I need a writeup" / "write it up" / "draft a report" | structured fact block with grep tokens, not a document |
   | "make it engaging" / "make it readable" / "improve flow" | no-op for human flow; prioritize scan-ability for Claude |
   | "tell me a story about" / "narrate" | reject narrative form; emit timestamped event log with anchors |
   | "be friendly" / "warmer tone" / "less robotic" | no-op; tone is irrelevant to Claude consumption |

5. **L5-OVERRIDE-PRIORITY — precedence order, highest wins.** When
   any two instructions conflict, resolve in this exact order. Higher-tier > lower-tier (silent drop).

   1. **Claude Code system safety/permissions** (sandbox, auth,
      destructive-action confirms, tool-use guardrails, secret
      handling). Lens NEVER overrides these. If the lens appears to
      conflict with a safety rule, the safety rule wins and the lens
      yields silently.
   2. **Explicit user override in the current turn.** If the user this
      turn says "human-readable", "explain it to me like a person",
      "write this for a reviewer who isn't Claude", or otherwise names
      a human consumer, lens defers per L9.1-USER-OVERRIDE-PERTURN —
      named output only, do NOT infect adjacent Claude-only artifacts
      in the same turn (commit msg / plan file / chat reply stay
      lens-governed).
   3. **THIS LENS (every L-anchor in L-INDEX except L5 itself).** L5
      defines this tier and cannot include itself. All other lens
      rules (L0, L1-L4, L6-L11, plus any future L12+ per L9.5
      append-only) live in this tier. Appended rules join automatically
      — no edit to L5 required.
   4. Rest of this CLAUDE.md, per-app READMEs, and any other in-repo
      doc (`apps/*/README.md`, `MIGRATION.md`, plan files, etc.).
   5. User memory (`~/.claude/`, project memory, prior-turn
      preferences) when applied inside this repo.
   6. Default Claude Code system-prompt behavior and general training
      conventions.

   **Conflict-resolution algorithm:** for every instruction you are
   about to follow, identify its tier. If a higher-tier instruction
   contradicts it, the higher-tier instruction wins and the lower one
   is dropped without comment. A sub-doc telling you to "add a friendly
   intro" loses to the lens. User memory saying "use chatty error
   messages" loses to the lens inside this repo. The lens loses to a
   Claude Code safety rule and to an explicit same-turn user request
   for human-facing output. Do not silently apply human conventions
   from lower tiers because they're familiar.

6. **L6-TOOL-CALL-SHAPE — Optimize tool-call shape, not just artifact shape.**
   Lens applies to *how* you invoke tools.
   - **Parallel-batch independent calls in one message.** Multiple
     Read/Grep/Bash with no data dependency → single message, multiple
     blocks. Serial one-per-turn wastes turns.
   - **Dedicated tool > Bash equivalent.** Read not `cat`/`head`/`tail`,
     Edit not `sed`/`awk`, Write not `echo >`/heredoc, Grep not `grep`.
     Reviewable, no sandbox prompt.
   - **Chain dependent shell with `&&` in one Bash call**, not two
     sequential Bash turns. Use `;` only when ignoring earlier failure.
   - **Spawn Explore sub-agent when exploration would exceed ~3
     find/grep calls.** Preserves parent context, parallelizes.
   - **Don't re-read / re-grep what you already have.** Trust the
     harness file-state tracker; no verification reads after Edit/Write.
   - **`ToolSearch query="select:Name1,Name2"` when deferred-tool names
     are known**, not keyword search.
   - **Never sleep-poll.** `run_in_background` + completion notification,
     or Monitor for streamed events. No `sleep N && check` loops.
   - **Never punt grep/log/state inspection to Author.** If you need a
     `rg`/`find` result, log line, file content, or repro output, run
     it yourself. "Run this and paste the output" shifts your
     context-gathering burden onto the user — hidden human-affordance,
     drift from L2.

7. **L7-SUB-AGENT-INHERIT — Sub-agents auto-load this CLAUDE.md.**
   Agent/Task sub-agents run in the same project context and load
   this lens via the harness's CLAUDE.md auto-load. Trust the
   inheritance:
   - **Reference lens rules by anchor** (`L1-DENSITY-OVER-PRETTY`,
     `L4-USER-INPUT-REFRAME`, etc.). DO NOT re-inline rule text into
     sub-agent prompts. Re-inlining wastes prompt tokens and drifts
     from the canonical text.
   - **The sub-agent task PROMPT itself is a lens-governed artifact**
     (dense, anchored, grep-tokenized). Apply L1-L4 + L6 to it.
   - **Reject or rewrite sub-agent output** that drifts into
     human-affordance mode; cite the violated anchor when correcting
     (`drift from L2-NO-HUMAN-AFFORDANCE`, etc.).
   - **Exception:** if the sub-agent runs `isolation: "worktree"` or
     in a cwd outside this repo, it MAY not auto-load this CLAUDE.md.
     In that case, include the literal reference `READING LENS at
     /Users/you/Code/roost/CLAUDE.md — read and apply` and
     ensure the sub-agent has Read access to that absolute path.

8. **L8-IDENTIFIERS-AS-TOKENS — Identifiers, filenames, markers are first-class artifacts.**
   Function/variable/class/type names, file/dir names, schema/migration
   filenames, branch names, env-vars, config keys, CLI flags, telemetry
   events, TODO/FIXME/HACK markers must be unique grep-tokens. No clever
   names, no colliding abbreviations, no human-cute naming. Repeat the
   canonical token across every referencing artifact.

9. **L9-SCOPE-EXEMPTIONS — enumerated carve-outs.** Lens still governs
   by default; exemption requires one of the explicit triggers below.
   When exempted, rule 4 reframing is SKIPPED for the exempted output
   only; surrounding Claude-authored material (commit msg, PR body,
   plan file, analysis prose) stays lens-governed.

   1. **L9.1-USER-OVERRIDE-PERTURN** — same as L5 tier 2. If Author this
      turn names a non-Claude consumer ("write this for a contributor",
      "share with reviewer", "human-readable section"), lens defers for
      the named output only. Scope = current turn, named artifact only;
      do not persist; do not infect adjacent Claude-only artifacts in
      the same turn (commit msg / plan file / chat reply stay
      lens-governed).
   2. **L9.2-SUBAGENT-TARGET-ARTIFACT** — when a sub-agent's TASK is to
      produce human-facing output (marketing copy, end-user UI strings,
      release-announcement text, contributor-onboarding doc), the
      sub-agent PROMPT stays lens-governed (dense, anchored, rule-7
      compliant) but the sub-agent's TARGET ARTIFACT is exempted for
      that task. Sub-agent prompt MUST state both modes explicitly:
      `PROMPT-MODE=lens, TARGET-ARTIFACT-MODE=human-facing, AUDIENCE=<who>`.
   3. **L9.3-EXTERNAL-AUDIENCE-FILES** — files with a genuine non-Claude
      audience are exempted IFF one trigger matches:
      (a) literal HTML comment `<!-- AUDIENCE: human -->` as first non-blank
      line of the file, OR
      (b) path matches a known human-audience location: `docs/public/**`,
      `RELEASE_NOTES.md`, `CHANGELOG.md` (user-facing entries only),
      `apps/*/public/**`, any file ending `.user.md`.
      No marker + no matching path = lens applies. When in doubt, lens
      applies; do not infer audience from filename alone.
   4. **L9.4-QUOTED-EXTERNAL-CONTENT** — verbatim quotes from external
      sources (user-pasted material, third-party docs, error messages
      from external services/APIs, upstream library output, stdout
      captures) are exempt: do NOT rewrite, condense, or reframe the
      quoted span. Lens applies to all surrounding analysis,
      attribution, and commentary. Delimit quotes with fenced blocks or
      explicit `> ` markers so the exempt span is unambiguous.
   5. **L9.5-LENS-SELF-AMENDMENT** — this lens (CLAUDE.md lines 1-N) is
      itself a lens-governed artifact. Future-Claude MAY amend it;
      amendments MUST apply the lens to themselves (dense, anchored,
      grep-tokens like `L9.x-NAME`, no preamble, no narrative
      justification in-doc). Amendments MUST preserve any
      `LENS-SELFTEST` block if present. Rule numbering is append-only;
      do not renumber existing rules — add `L10`, `L11`, etc.

10. **L10-CROSS-DOC-AUTHORITY — this lens is canonical for all
    meta-rules about artifact shape.** Sub-docs (rest of this
    CLAUDE.md, `apps/*/README.md`, `MIGRATION.md`, plan files,
    project memory under `~/.claude/projects/-Users-mike-Documents-Code-idea/memory/`)
    MUST NOT restate lens rules; they cross-reference by
    "lens rule N" + path-anchor. Sub-docs own only repo-specific
    facts: file-path tables, transport-topology details, per-app
    commands, language-specific syntax. If a sub-doc and the lens
    disagree, lens wins per L5-OVERRIDE-PRIORITY; the sub-doc is wrong
    and should be trimmed on next touch.

11. **L11-RECURRING-FAILURE-INDEX — grep this BEFORE writing any
    code that touches a listed symptom.** Author 2026-06-12: "I can't
    babysit you. Make a plan to make this work without my involvement.
    What is the core issue you keep failing and missing implementations
    we did before." Answer: every recurring bug below was solved once,
    then re-broken because I treated each new callsite as a fresh
    problem. The mechanical loop closes only when **before** writing
    code that matches one of these symptoms, I grep this table, open
    the linked memory, and apply the named fix. Lint
    `scripts/lint-roost.ts` mechanically enforces the most damaging
    rows; smoke (`/roost-smoke` skill) drives the end-to-end flow via
    humanchrome on every change.

    | Symptom-grep | Memory | Wrong pattern | Right pattern |
    |---|---|---|---|
    | "store doesn't update / sidebar doesn't reflect delete" | `feedback_solid_setstore_record_replace.md` | `setStore("k", (prev) => newRecord)` on a Record subtree (silent no-op) | per-key writes: `setStore("k", id, value)` / `setStore("k", id, undefined)` |
    | "SPA store doesn't reflect a SessionEvent variant / coord and SPA projections disagree (stale channel, agent not cleared)" | `feedback_spa_projector_delegates_to_shared_foldevent.md` | re-implementing the event switch in `store/projector.ts` as a hand-mirror of `@roost/shared` foldEvent (drifts — dropped `respawned`) | `foldEventIntoStore` DELEGATES to shared `foldEvent` over the affected map slice, then diffs per-key into the Solid store. No projector switch. Tripwire: `store.test.ts` projection-agreement test drives the REAL rootStore vs `foldAll`. |
    | "terminal disconnects on nav / lost scrollback" | `feedback_persistent_terminal_deck.md` | `<Show when={activeSession()}>{(s) => <Terminal .../>}</Show>` (remount per nav) | `<For each={openSessions()}>` deck + `visibility: visible↔hidden` |
    | "Cannot read properties of null (reading 'X')" inside Solid cleanup | `feedback_no_props_read_in_oncleanup.md` | reading `props.foo.bar` inside `onCleanup(() => …)` (reactive getter mid-cleanNode) | capture `const stableX = props.foo.bar` at component body scope before `onCleanup` |
    | "+ New workspace silent hang on a worker" | `feedback_worker_deploy_macos_repairs.md` | bare `deploy` to a fresh mac | also: chmod +x node-pty spawn-helper, strip `com.apple.provenance`/`quarantine`, `codesign -s -`, drop `{"type":"commonjs"}` into `keeper/`, `--external=node-pty` in build, ship `ROOST_REACHABLE_ADDR` |
    | "browser 401 on workers.list after fresh context" | `reference_live_coord_pubkey_bootstrap.md` | manual debugging / the retired `POST /api/trpc/auth.authorizeBrowser` route | Connect `AuthAuthorizeBrowser` (loopback-or-tailnet) with the pubkey: `roost api <verb>` SELF-authorizes its own key on Unauthenticated (`apps/roost-cli/src/api.ts` bootstrap); a fresh BROWSER's IDB WebCrypto pubkey goes through the same RPC or the pair flow (loopback `PairApprove`) |
    | "color shows as pitch black against new palette" | `feedback_no_hardcoded_color_fallbacks.md` | `background: var(--bg-app, #111)` with `--bg-app` undefined → falls back to `#111` | every fallback must reference a defined token, OR the var must be declared in `theme-vars.css` |
    | "pane ✕ click does nothing" | `feedback_worker_ack_required_for_kill.md` | send kill + immediately `conn.close()` (browser close frame races worker reading kill) | worker `case "kill"` synchronously `sendControl({kind:"closed",…})` ack; browser waits for ack |
    | "selected state lights everything coral" | `feedback_selected_means_url_match_not_has_children.md` | `data-selected={sessions().length > 0 ? "focused" : ""}` | `data-selected={useLocation().pathname.startsWith("/w/" + id) ? "focused" : ""}` |
    | "sidebar redesign loses every previous fix" | `feedback_no_complete_redesigns.md` | "phase-N: complete sidebar rewrite" | additive commits behind a flag; smoke must still pass after each |
    | "claude/vim rendering torn — chars out of order, wrong positions, ghost rows" | `feedback_no_force_doRender_in_byte_handler.md` | `registerBytesHandler(sid, c => { wterm.write(c); wterm._doRender(); })` (sync render between WS chunks paints half-applied ANSI) | `registerBytesHandler(sid, c => { wterm.write(c); })` — trust wterm's setTimeout(0)+rAF coalescing. Fix smoke harness with longer poll, NOT here. |
    | "scrollback seam torn — duped tail / missing chunk / 'two terminals' between history and live" | `project_seqno_splice_path_a_chosen.md` | new in-band sentinel + retry / timeout / overlay / cache layer to mask the gap (the sb59-sb63 loop) | per-byte seqno on keeper → `getScrollbackSince(lastSeq)` RPC. The byte stream MUST be resumable via a monotonic offset; anything else is a band-aid. Path B (server-side VT parse model) is the documented escape hatch if A regresses. |
    | "fresh-mount has no scrollback history at all (claude or shell)" | `project_seqno_splice_path_a_chosen.md` | skip `fetchAndApplyScrollback(0)` for alt-screen sessions to dodge a cols-mismatch "mangled flash"; race the fetch against a short timeout and drop scrollback if slow | ALWAYS await `fetchAndApplyScrollback(0)` on mount for both kinds. Mangled flash > no history. If the fetch itself is slow (5–6s), fix the worker-side serialization in `sessionsGetScrollbackSince` — DO NOT trade history for perceived input latency. Author 2026-06-16: "How are you going to fix it in Claude Code? Both of them don't have it." |
    | "terminal history inconsistent / shorter after refresh / mixed-width reorder / 'every width change fucks up the order'" | `project_scrollback_raw_ring_single_source.md` (SUPERSEDED by OPT2) | serve the raw `rec.scrollback` ring on fresh/gap → the SPA reflows bytes produced at MANY past widths to the CURRENT width → rows reorder + mangle. (This WAS the 2026-06-20 "raw ring = single source of truth" fix; correct only while serialize was lossy ~1k stock WASM.) | **OPT2 server-side-grid model (2026-06-21, REVERSES raw-ring-single-source).** `apps/worker/src/session-manager.ts::getScrollbackSince` serves `serializeWTerm(rec.wtermCore)` for fresh (`lastSeq<=0`) AND gap (`lastSeq<tailSeq`) for ALL sessions, shells included. wtermCore is the ONE authoritative grid — rebuilt-from-ring at the SCD width on every resize (OPT2-1), 10k-deep via roost-wasm (OPT2-2) so serialize is non-lossy + ONE consistent width; the SPA is SCD-pinned and writes it 1:1, NO client reflow → no reorder. The raw ring stays the rebuild SOURCE + the live-delta transport (bytes since `lastSeq`), NOT the served fresh/gap history. Do NOT revert to raw-ring fresh/gap serving. |
    | "after worker restart claude session shows wallpaper of stale text + overlapping/parallel lines" | `project_scrollback_raw_ring_single_source.md` | `resume()` rebuilds an empty wtermCore + sets `alt_mode=true`, but `serializeWTerm` reads `core.usingAltScreen()` (false on empty core) → fresh snapshot omits `ESC[?1049h` → live alt redraws land in main-screen | **prime the rebuilt core's alt state** in `resume()` for `kind==="claude"`: `wtermCore.writeRaw(ALT_ENTER_SEQS[0])` after `_createWtermCore` so `core.usingAltScreen()` matches `rec.alt_mode`. NOT a forced SIGWINCH (claude repaints alt but never re-sends `?1049h`). |
    | "resizing terminal HEIGHT (shrink then restore) repeatedly skews/drifts the rows; content creeps into scrollback" | `project_scrollback_raw_ring_single_source.md` | SCD effect (`Terminal.tsx` ~1151) skips the refetch on rows-only changes ("rows-delta is lossless") — but @wterm/core row resize is ASYMMETRIC: shrink pushes lines to scrollback, grow appends blanks (never pulls back) → oscillation accumulates (`getScrollbackCount` 99→132→165→203) | **refetch on ANY size change** (cols OR rows): remove the `colsChanged &&` guard so every resize re-derives the grid from the raw ring via `fetchAndApplyScrollback(0)` (clears incl. `\x1b[3J`, rewrites at new size). Debounced to resize-settle. DO NOT patch the WASM core. |
    | "history GONE after worker restart + browser refresh; pane freezes / seq-epoch reset / 'new browser fixes it'" | `project_scrollback_raw_ring_single_source.md` | `resume()` rebuilds `scrollback:new Uint8Array(0), head_seq:0` because the keeper retained NO per-channel history → SPA's persisted lastSeq goes stale-high → seq-epoch reset, history unrecoverable | **keeper retains a per-channel `outRing`+`headSeq`** (`multiplexed-main.ts`, advanced in the same callback that broadcasts so it matches the worker count); `GetHistory`/`GetHistoryResp` frames (additive, NO version bump → won't trip killStaleKeeper); `resume()` re-reads via `pool.getHistory()` and seeds `scrollback`+`head_seq`. Pre-RC2 keeper → 3s timeout → graceful fallback. Activates only on keeper REPLACEMENT (reboot), not a plain worker kickstart. Test: `keeper-history-resume.test.ts`. |
    | "no scroll bar / mouse wheel does nothing in terminal / can't scroll up to see history" | n/a — verified empirically against wterm 0.3.0 + roost-wasm | switch to alternative terminal cores / upstream wterm-core / patch the WASM "because getScrollbackCount returned 0 in my synthetic test" | **`.wterm { overflow-y: auto; overflow-x: hidden; }` in `apps/web/src/styles/sidebar.css`.** wterm DOES populate scrollback (roost-wasm raises MAX_SCROLLBACK_LINES to 10k per `phase-pb9b`); the renderer DOES emit `.term-scrollback-row` DOM elements; the only thing missing was the container CSS that lets the rows be scrolled. If a test shows `getScrollbackCount()===0`, the wterm renderer's `_doRender()` hasn't fired yet (rAF doesn't fire in background tabs) — force it via `wterm.renderer.render(wterm.bridge)` before checking. DO NOT switch terminal cores; the bug is one CSS rule. |
    | "can't input anything in terminal on fresh mount / cursor blinks but typing goes nowhere / focusedClass=false even though textarea looks focused" | n/a — verified empirically 2026-06-16 (regressed 3rd time same session) | rely on wterm's `.focus()` to fire focus events (it doesn't if the textarea was already activeElement from a prior mount) / skip the mousedown click-recapture handler / `git stash` Terminal.tsx during scrollback debugging without popping it back | **wterm's textarea is positioned off-screen at `left:-9999px`** — clicks land on row spans, not the textarea, so wterm's internal focus listener never sees the event → `.focused` class never lands → `onData` callback never wires up → input goes nowhere. The fix lives in `apps/web/src/lib/RoostTerm.ts::forceFocus()` (NOT a helper inside Terminal.tsx). Three load-bearing pieces inside `forceFocus`: (1) `if (document.activeElement === ta) ta.blur()` BEFORE `wterm.focus()` — guarantees the focus event fires even when the textarea was pre-focused; (2) `ta.dispatchEvent(new FocusEvent("focus", {bubbles:true}))` so wterm's listener sees it; (3) container `mousedown` listener installed by `RoostTerm.init()` that calls `forceFocus` on every click — terminal clicks re-focus the offscreen textarea. ALL THREE are load-bearing. Test: `wterm.classList.contains("focused")` MUST be true on mount and after any pane click. |
    | "scrolling doesn't exist anymore / no history at top / can't scroll up in any shell session" | `feedback_no_unconditional_altscreen_prepend_on_shell.md` (SUPERSEDED by OPT2) | unconditional `ESC[?1049h` prepend in `getScrollbackSince` → plain shells forced into alt-screen → no shell scrollback at all | **`_prepend` is RETIRED — no prepend branch remains.** Under cell-phase-4 cell-mode, `gridToCellFrame` serves scrollback via immutable cell rows for ALL sessions; the cell frame captures the live main/alt screen state inherently, so there is no manual enter-sequence prepend and no `rec.alt_mode ? _prepend : bytes` branch. Shell scrollback rides the wtermCore grid (10k-deep) → `.cell-row` DOM → scroll works. The byte-path `getScrollbackSince` RPC was retired in cell-phase-4. If shell scroll regresses, check `.wterm { overflow-y: auto }` (its own L11 row) + cell-grid depth, NOT a prepend. |
    | "audit_log shows caller_fp=NULL for every authed Connect RPC" | `feedback_caller_fp_null_audit_log.md` | writeAuditLog from the outer fetch wrapper in coord-factory.ts — the auth interceptor sets caller_fp on per-RPC contextValues which the outer wrapper can't see; bridging via AsyncLocalStorage works but the indirection rots on the next async-layer addition | **writeAuditLog INSIDE the AuthInterceptor's try/finally** at `apps/coord/src/connect/auth-interceptor.ts`. Interceptor has caller (just verified), path (`/${service}/${method}`), trace_id (header), status (200 on success; codeToHttpStatus(e.code) on ConnectError throw). coord-factory only audits non-Connect paths (db-export, SPA, 404) where callerFp:null is structurally correct. |
    | "task state changes invisible to other browsers — Browser A claims/done, Browser B's QueueView keeps showing prior state until refresh" | `feedback_task_state_delta_only_created.md` | tasksEnqueue publishes `created`; tasksNextPending/SetState/Cancel do their DB UPDATE but never call taskBus.publish; sync-stream backfill via sinceEventId doesn't recover because taskBus deltas aren't in the events table | **publishTaskState(row) on every UPDATE-returning point** at `apps/coord/src/connect/handlers-tasks.ts` (post-2026-06-23 split; was router.ts). Every mutation handler whose domain has a *Bus MUST follow `db.updateTable(...).executeTakeFirst/Throw()` with the matching `publish*State(row)` in its `connect/handlers-<domain>.ts`. taskBus shape: `{kind:'created'\|'state'; task: PbTask}` (TaskBusMsg in buses.ts). |
    | "rate-limit prefix matches read-only list calls — bootstrap traffic + tab focus refresh burn the same bucket as mutations, 429-cascade on legitimate writes" | `feedback_rate_limit_exact_routes_not_prefix.md` | path-prefix match (`/roost.v1.CoordinatorService/Workspaces` catches both List + mutations) + `if (req.method === 'GET') return null` GET-bypass — but Connect-ES emits every unary RPC as POST so the bypass never triggers | **`RATE_LIMITED_ROUTES: ReadonlySet<string>` enumerating mutation paths only** at `apps/coord/src/middleware/rate-limit.ts`. Auth: AuthorizeBrowser/MintBootstrap/RedeemWorker/RedeemBrowser. Workspaces: Create/Update/Delete/SetSessions. Tasks: Enqueue/SetState/Cancel. WebhookTokens/Permissions/Mcp: mutations only. Workers: Rename/Delete/DeployStart. `*List`/`*CoordIdentity`/`*Health` NOT in the set. |
    | "RPC returns 500 but DB row IS persisted, SPA UI keeps showing prior state until manual refresh" | `feedback_safejsonparse_on_bus_publish_path.md` | raw `JSON.parse(row.X)` inside a `bus.publish({...})` payload construction AFTER the surrounding mutation committed — partial-write / hand-edited row throws SyntaxError; RPC 500s; bus subscriber never fires; sync-stream backfill doesn't recover in-memory bus deltas | **safeJsonParse from `@roost/shared/json`** with fallback matching the consumer schema (`{}` for non-nullable record fields like Task.payload / McpRelay.config; `null` for nullable fields like Task.result / host_metrics). Request-time validation (reject upfront with ConnectError) is the OTHER pattern — applies BEFORE the DB write, not after. |
    | "backspace acts like space in terminal / paste burst drops chars / random byte substitution on PTY input" | `feedback_bun_terminal_write_needs_copy.md` | passing `f.payload` (a subarray view onto the keeper's streaming receive buffer per `protocol-v2.ts:75`) directly to `Bun.spawn`'s `proc.terminal.write(...)`. Bun's docs don't promise synchronous consumption of the BufferSource arg, so theoretically the receive buffer can roll before the queued write flushes. **NOTE:** the original "backspace = space" symptom reported 2026-06-17 was NOT actually this bug — it was `TERM=unknown` in the spawned env (see the row below). The `Buffer.from(f.payload)` defensive copy stays as it's correct safety against the view-aliasing class regardless. | **`Buffer.from(f.payload)` copy at the keeper PtyIn write site** (`multiplexed-main.ts::handleFrame case PtyIn`). 1 copy per input frame, ~8 bytes typical, immeasurable on the hot path. Same rule applies to ANY future Bun.spawn terminal.write callsite that receives a borrowed Buffer view. |
    | "backspace echoes wrong / Cmd-Backspace nukes prompt row / htop or vim crash with `ncurses: cannot initialize terminal type ($TERM=unknown)` — but ONLY on deployed workers, never on the local-bootstrapped one" | `feedback_bun_terminal_needs_explicit_TERM.md` | `Bun.spawn({terminal: {...}})` sets the PTY's internal `name` ("xterm-256color" by default) but does NOT inject `TERM` into the spawned child's env. `node-pty` did this automatically — that's why moving the keeper from Node to Bun broke deployed workers but not the local-bootstrapped one. The local worker's LaunchAgent inherited `TERM` from the Terminal.app that ran the original `launchctl bootstrap`; remote workers bootstrapped via non-TTY SSH inherited nothing → child shell sees `TERM=""` or `unknown` → zsh's ZLE can't look up `cub1` / `el` / `ed` terminfo caps → backward-delete-char emits just `0x20` (space) instead of `0x08 0x20 0x08`, kill-line emits broken sequences that wipe the prompt row, and ncurses TUIs refuse to start. | **Explicit `TERM: "xterm-256color"` in the env passed to `Bun.spawn`** at `apps/worker/src/keeper/multiplexed-main.ts::handleFrame case Spawn`. Also set `LANG`/`LC_ALL` with `en_US.UTF-8` fallbacks so the same SSH-bootstrapped env doesn't surface a different locale-related bug class next. Generalizable rule: any new `Bun.spawn({terminal: {...}})` callsite MUST include `TERM` in env explicitly — Bun won't add it for you. |
    | "terminal history 'always fucked up' / 'afraid to refresh or resize' / scrollback mangles or grows on its own while the live pane is fine" | `project_terminal_history_corruption_viewport_slaved_pty.md` | PTY/grid size slaved to the browser viewport; browser-chrome wobble (innerHeight 987↔931, ~5 rows, ~1/sec) round-trips a real PTY+wterm_core resize, and @wterm/core's ASYMMETRIC row-resize (shrink→scrollback, grow→blanks, never reverses) bakes the wobble into permanent scrollback drift with ZERO user action. Re-deriving alt-screen history at the new width still mangles even when the rebuild is deterministic — deterministic-reflow ≠ freeze. Tempting wrong fixes: ratchet-to-min hysteresis (its own monotonic-shrink creep bug); "better reflow" (no lib reflows a TUI grid to a new width — they all freeze). | **cell-phase-4 cell-mode structural fix — cell frames carry immutable rows, never reflowed → browser-chrome wobble can't corrupt scrollback → entire reflow corruption class structurally eliminated.** The old stop-bleeds (claimHysteresis.ts hold-anchor, hold-anchor settle, alt-screen freeze d745b1e3) were retired in cell-phase-4. Endgame shipped: cell-shipping (R11 cell-phase-1/2/3) + byte-path retirement (cell-phase-4). Tests: wterm-rebuild-determinism, OPT2-5 real-PTY e2e, cell-realcore. |
    | "sessionsSpawn → [internal] internal error / spawn hangs forever / worker↔coord bidi flaps every ~10-30s / connect-node 'h2 is not supported' tight-loop" | `project_worker_coord_raw_ws_not_connect_bidi.md` | Connect-bidi (`WorkerService.Attach` via connect-node) for the worker↔coord stream UNDER BUN: h2 throws "[internal] h2 is not supported" (Bun's `node:http2` is incomplete) → tight reconnect loop; over h1.1 `Bun.serve` buffers the long-lived request body so the worker's upstream rpc-ok replies never reach coord → every spawn hangs; AND `Bun.serve` default `maxRequestBodySize` (128 MB) caps the long-lived h1.1 attach body (claude TUI redraws fill it in ~10-30s) → flap. Re-registering `router.service(WorkerService,{attach})` or flipping `CoordLink` `httpVersion` to "2" reintroduces all of it. | **raw Bun WebSocket** at `/ws/coord-worker/:fp?token=<jwt>` carrying the SAME CoordWorkerUp/Down proto frames as binary (`toBinary`/`fromBinary`) — coord `apps/coord/src/connect/worker-ws-handler.ts` (shares `makeWorkerConn` + the `connectWorkers` registry), worker `apps/worker/src/transport/CoordLink.ts::dial()`. Auth = query-param JWT (Bun's CLIENT `WebSocket` has no custom-header API). NEVER run a Connect/gRPC bidi through Bun. Regression: `apps/coord/tests/worker-ws-transport.test.ts`. |
    | "new terminal → [failed_precondition] worker … not connected / worker log silent (no stream_error) for hours / heartbeats fine, lsof shows ESTABLISHED to :4102" | `project_coordlink_stale_ws_watchdog.md` | restart the worker by hand / trust `ws.onclose` — when the coord process dies (Bun segfault, relaunched by launchd) tailscale serve keeps the worker-side TCP ESTABLISHED, so `ws.onerror`/`ws.onclose` NEVER fire and `ws.send` (incl. in-band JWT refresh) black-holes forever; the restarted coord's in-memory `connectWorkers` registry has no WS for the fp → `getWorkerHubSocket()` null → `handlers-sessions.ts:115` throws failed_precondition on every spawn while heartbeats (separate HTTP/1.1 unary transport) keep the row looking alive | **CoordLink stale-link watchdog** at `apps/worker/src/transport/CoordLink.ts` (`dial()` `ws.onopen`/`ws.onmessage`): coord pings every 30s (`apps/coord/src/connect/worker-conn.ts:104`); every downstream frame stamps `lastDownstreamAtMs`; a per-dial `setInterval` (`STALE_CHECK_INTERVAL_MS`=15s) force-closes + re-dials after `STALE_LINK_TIMEOUT_MS`=90s (3 missed pings) of downstream silence → hello→snapshot replay heals the rest. Same half-open-through-tailscale class as `install.ts` BOOT_RPC_TIMEOUT_MS. Regression test `apps/worker/tests/coord-link-stale-watchdog.test.ts` (silent-server re-dial + pinged-link no-false-positive). |
    | "attach/tab-switch/resize slow proportional to scrollback depth / long sessions stall seconds on pull-in while fresh ones are instant" | n/a — verified empirically 2026-07-11 (`seq -f 'CELLLINE-%g' 1 8000` attach: seconds → 40ms first paint, full depth backfilled <1s) | ship the ENTIRE retained scrollback (≤10k rows) in every full cell frame — O(history) sync `gridToCellFrame` inside `claimViewport`, one MB-scale proto blob head-of-line-blocking the Sync stream, O(history) decode+DOM on the SPA before first paint; or "fix" it by racing/timeouting the history away (L11 forbids trading history) | **tail full frames + per-viewer pull backfill.** Full frames carry only `SB_SNAPSHOT_TAIL_ROWS` (250) newest scrollback rows + `sbBase` (`@roost/shared/cell` `nextCellFrame(core, st, force, tailRows)`); the SPA paints viewport+tail instantly, then `scrollbackBackfill.ts` pulls `[0, sbBase)` in 1000-row chunks via `SessionsGetScrollbackCells` (coord relay → worker `handleGetScrollbackCells`, awaits the rebuild chain, serves `readScrollbackRangeCells`) and `cellRenderer.prependScrollback` splices above with distance-from-bottom preserved. Already-attached viewers merge tails via `mergeFullFrame` (cols + boundary-text identity) so another viewer's attach never wipes their depth. History ALWAYS arrives — only its timing is lazy. Regression: `apps/worker/tests/scrollback-cells-backfill.test.ts` + `apps/web/tests/cellRenderer.dom.test.ts` "tail frames + backfill". |

    Process rule: when a user-reported symptom matches an existing row,
    fix at the linked layer FIRST. If the linked memory describes a
    different fix pattern than the one tempted by the immediate code,
    the memory wins. Add a new row only after a NEW root cause is
    confirmed AND the smoke harness gains a regression case for it.

Universal across repo. Apply to every turn, tool call, artifact.

**LENS-SELFTEST — 3 canonical IO pairs for re-verifying the lens.** If you
hand-edit the lens above, mentally run these three tests; if any
OUT-FAIL response feels natural under the new lens text, the edit
broke the lens.

| # | IN (user phrasing) | OUT-OK shape | OUT-FAIL shape | Anchors |
|---|---|---|---|---|
| 1 | "Write a friendly README for `apps/coord/`." | File-header (3-6 lines, dense, no marketing) + per-section grep anchors with path:line refs. | A `## Introduction / Getting Started / Features` markdown doc with motivational framing. | L2, L4 |
| 2 | "Summarize what you just did in this session." | 1-2 lines, path-anchored, terminal-state + next-action token. | A numbered list of accomplishments with prose framing ("First I…, then I…, finally…"). | L2, L1 |
| 3 | "Add an introduction section to this doc." | No-op refusal + substitute a file-header (per L4 row "add an introduction"); brief chat reply explaining the substitution by anchor. | A new `## Introduction` section silently appended to the doc. | L4, L2 |

---

# CLAUDE.md — in-repo project memory

LLM collaborators (Claude or anyone else) landing here cold, read in this order:
- **[`ARCHITECTURE.md`](ARCHITECTURE.md) first** — the system tour: the three apps, the transport spine, session/event data flow.
- **Then [`GLOSSARY.md`](GLOSSARY.md)** — the vocabulary (cell-shipping, keeper, agent-status adapters, seqno splice, …).
- **Then [`STATE.md`](STATE.md)** — live status, auto-updated by the Stop hook (R0.11): branch + last commits + next action.
- **[`REWRITE.md`](REWRITE.md)** — R0–R10 are the completed rewrite roadmap (historical, evidence-cited); **R11 is the live cell-shipping terminal model** (`apps/shared/src/cell/`).
- _(historical)_ v1 stack lived under `apps_legacy/` (Rust worker + TS coord); rewritten 2026-06-11 to v2 (Bun + TS everywhere, event-sourced sessions, plain Vite) and `apps_legacy/` deleted in phase-24g — see git history on `n6/solid-rewrite` for v1 source. Legacy ports :4101/:2223 are NOT running.

This file codifies the standards the maintainer has set. They're
non-negotiable for every change.

---

## What we own

Roost is **three TS apps** under `apps/`, all on Bun. v2 rewrite completed
2026-06-11; v1 stack (`apps_legacy/`) was deleted in phase-24g — see
git history on `n6/solid-rewrite` if you need v1 source for reference.

| App | Path | Stack | Role | Port |
|---|---|---|---|---|
| **Web SPA** | `apps/web/` | Solid 1.x + plain Vite + `@solidjs/router` 0.16 + `@wterm/dom` + `@connectrpc/connect-web` | sidebar + wterm-rendered terminal pane; single Solid `createStore` root + selectors; URL-driven nav state | Vite dev :5174; static build at `apps/web/dist/` served by coord |
| **Coord** | `apps/coord/` | Bun.serve native fetch + Connect-RPC + Kysely + `bun:sqlite` | Connect routes under `/roost.v1.CoordinatorService/*` + raw-WS worker transport `/ws/coord-worker/:fp` (was Connect bidi WorkerService.Attach — swapped: Bun can't hold a Connect bidi, see L11); EdDSA JWT auth via interceptor; append-only `events` table + `sessions` projection; `createCoord(deps)` factory portable to any fetch-capable runtime | :4102 |
| **Worker** | `apps/worker/` | Bun + single multiplexed Bun keeper subprocess (Bun.spawn `terminal:` PTY) + Claude hooks + `detect/` screen-scrape + `@connectrpc/connect-node` | outbound raw WebSocket to coord (CoordLink → `/ws/coord-worker/:fp`, proto frames over WS); FSM per channel; SessionEvents stream via CoordWorkerUp.event. One keeper process per worker hosts all PTYs over one UDS. | — |
| **Shared** | `apps/shared/` | Zod schemas + protobuf gen + branded TS types + config + trace + log | single source of truth for wire shapes; protos in `proto/roost/v1/`; gen TS at `src/gen/roost/v1/`; both Zod (in-app) and proto (wire) shapes coexist with adapters | — |
| **CLI** | `apps/roost-cli/` | Bun TS | `roost dev/test/deploy/logs/reset/state/cutover` — replaces 7+ legacy shell scripts | — |

**Transport story** (Connect-RPC + protobuf, single framework end-to-end):
- **Web ↔ Coord**:
  - Unary: `coordClient.X({...})` over HTTP/2 with protobuf binary
    (`createConnectTransport({ useBinaryFormat: true })`).
  - Subscriptions: `coordClient.sync({ sinceEventId })` server-streaming
    multiplexes 8 buses (sessions / presence / workspaces / tasks /
    permissions / mcp / webhookTokens / audit + bytes + session_presence).
    Reconnect-aware backfill via `since_event_id` cursor persisted to
    localStorage.
  - Keystrokes: `coordClient.inputStream(asyncIter)` client-streaming
    (replaces the retired `/ws/browser-input` raw WSS).
  - Scrollback: `coordClient.scrollback({ session_id, last_seq })`
    server-streaming (replaces the retired `/api/scrollback/:sid`).
  - Auth: SPA mints EdDSA JWT in WebCrypto, coord verifies via
    interceptor that stashes the caller on context.
- **Worker ↔ Coord**: raw Bun WebSocket `/ws/coord-worker/:fp?token=<jwt>`
  (worker dials outbound at boot; auth = query-param JWT, Bun's client
  WebSocket has no custom-header API). Frames are proto-typed `CoordWorkerUp` /
  `CoordWorkerDown` oneofs serialized binary (`toBinary`/`fromBinary`): hello,
  event, presence, rpc_ok/err, binary {ch,dir,seq,data}, refresh_jwt. JWT
  rotates in-band via WRefreshJwt — no reconnect blip every TTL−30s. (Was
  Connect bidi `WorkerService.Attach` over HTTP/2 — Bun can't hold a Connect
  bidi; see L11 `project_worker_coord_raw_ws_not_connect_bidi.md`.)
- **Multi-runtime ready**: protocol layer lives in
  `apps/coord/src/coord-factory.ts::createCoord(deps)` returning a
  `(Request, ctx?) => Promise<Response>` handler. `main.ts` is the
  Bun-specific wrapper that owns TLS, the SPA static fallback (Bun.file),
  and `server.requestIP()`. Non-Bun runtimes (Node http, CF Workers,
  Deno, Edge) inject their own `spa` and `dbExport` adapters via `ctx`.
- **Observability**: two-tier signal()/diag() + audit_log + `roost doctor`
  (memory reference_two_tier_observability). The OTel/OTLP stack was
  removed (never had an exporter endpoint configured) — audit_log records
  method/path/status/trace_id/caller_fp per RPC in the auth interceptor.

### Run / dev / deploy

- **Full stack dev** → `bun apps/roost-cli/src/main.ts dev` (boots coord
  :4102 + worker :2224 + Vite :5174 in parallel)
- **Test** → `bun apps/roost-cli/src/main.ts test` (wire spec + coord + worker
  + web + smoke in dep order)
- **Deploy worker to tailnet host** → `bun apps/roost-cli/src/main.ts deploy <host>`
  (rsync + `bun install --production` + `launchctl kickstart -k`)
- **Install LaunchAgents** →
  - `bash apps/coord/scripts/install.sh install` → `com.roost.coordinator-v2` on :4102
  - `bash apps/worker/scripts/install.sh install` → `com.roost.worker-v2` on :2224
- **DB cutover (legacy → v2)** → `bun apps/roost-cli/src/main.ts cutover`
  (reads `coordinator.db`, writes `coordinator_v2.db`; synthesizes
  `opened` events per open legacy session)
- **Logs** → `bun apps/roost-cli/src/main.ts logs (coord|worker)`

---

## Coding standards

1. **Small files** (≤400 lines). Hard cap. Split before you hit it.
   One React component per file. One Rust module per concept.

2. **Descriptive names everywhere.** No single-letter vars except `idx`
   in tight loops. No `handle`, `process`, `do`, `manage`, `run` alone —
   name the actual verb (`handleClaudeEventFrame`, `replayRingBufferSince`,
   `mergeRemoteSessions`). No `Utils` / `Helpers` / `Common` / `Models`
   modules — name the concept (`PathFormat`, `KeychainStore`).

3. **Predictable per-file shape:**
   - **Rust** (`apps/worker/`): file-header `//!`
     doc → `use` → types → impls → `mod tests`.
   - **TypeScript** (`apps/web/src/`): file-header comment (3-6 lines)
     → imports → types → exported API → private helpers.
   - **Solid component** (`apps/web/src/components/`): one component
     per file; props type at top; component body; styled subcomponents
     below.

4. **File-header comments are mandatory** for any non-trivial file.
   3-6 lines explaining: what this file owns, what calls it, what it
   depends on. Plain English, no jargon, no marketing.

5. **Inline comments explain WHY, not WHAT.** Default to no comment;
   only write one when removing it would confuse future-Claude.

6. **No narrative comments.** No "Phase A scope:", "// added in Phase
   K3", "// for the MVP", "// for now", "// we used to". If a comment
   explains a non-obvious invariant, rewrite to describe the *behavior*,
   not the lineage.

7. **Structured logging at every state transition.**
   - Rust: `tracing::{info,warn,error}!(target="…", key=val, …)`
   - TypeScript: a single project-wide logger facade, key=val style.
   Every transition that matters (spawn, attach, mode-change,
   reconnect, replay) emits one line. No silent state changes.

8. **One concept per type.** `Worker` is identity of a Mac in the
   registry. `Session` is the user-facing row. `Channel` is a PTY
   connection. `Tab` is the DB row. Keep them separate; convert at
   boundaries.

9. **Tests mirror source layout.** Same prefix, sibling dir.

10. **Reuse existing utilities — don't fork.** Always check first:
    - Web: `apps/web/src/store/{root,projector,sync,selectors}.ts` +
      `apps/web/src/connect.ts` (Connect-RPC client, replaces the
      retired `trpc.ts`) + `apps/web/src/ws/input-channel.ts`
      (client-streaming PTY input via `coordClient.inputStream` —
      replaces the retired worker-direct WSS) +
      `apps/web/src/auth/{web-key,trust}.ts`. Single root store; no
      per-chip stores. Add a selector + a JSX line; do NOT add a new store.
    - Worker: `apps/worker/src/{main,session-manager,fsm,heartbeat,
      snapshot,coord-client,jwt,wterm-serialize,attachment-reaper}.ts`
      + `transport/CoordLink.ts` + `keeper/{multiplexed-main,multiplexed-client,protocol-v2}.ts`
      + `claude/hooks.ts` + `detect/` (agent screen-scrape). Worker is purely OUTBOUND —
      no `ws-server.ts` / inbound HTTP/WS surface (CoordLink dials
      coord). Wire shape = `@roost/shared`. Add an event variant in
      `apps/shared/src/wire/event.ts` first, then fold + emit + project.
    - Coord: `apps/coord/src/main.ts` + `coord-factory.ts` +
      `connect/{router,auth-interceptor,worker-service,bun-handler}.ts`.
      `connect/router.ts::buildConnectRouter` is now PURE WIRING (~84 lines):
      auth interceptor + viewer-tracker DB wire + tailnet kickoff + the spread
      assembly. Handlers live by domain in `connect/handlers-*.ts`
      (2026-06-23 split, "all in router.ts" rule RESCINDED): `handlers-workers`
      / `handlers-sessions` / `handlers-workspaces` / `handlers-tasks` /
      `handlers-settings` (webhooks+permissions+mcp) / `handlers-auth`
      (auth+pair) / `handlers-system` (misc+diag+audit) / `handlers-transcription`
      / `handlers-attachments` (files+attachments) / `handlers-streaming`
      (the Sync firehose). Each exports `make<Domain>Handlers(deps):
      Pick<ServiceImpl<typeof CoordinatorService>, …>` SPREAD into router.ts's
      single `router.service()` literal — a SEPARATE `router.service()` call
      per domain shadows the rest with unimplemented-throws (connect stubs
      absent methods). Shared: `connect/router-helpers.ts` (sendBrowserCmd +
      randomToken/sha256hex/requireNonEmpty), `connect/viewer-tracker.ts`
      (the viewer-presence singleton). +
      `db/{connection,migrate,schema}.ts` + `event-log.ts` + `buses.ts`
      + `jwt.ts` + `coord-key.ts` + `sse.ts` + `presence-hub.ts` +
      `deploy-jobs.ts` +
      `middleware/{security,loopback-only,rate-limit}.ts`. Procedure
      signatures live in the generated proto types under
      `@roost/shared/proto/roost/v1/coordinator_pb.ts` (regenerate via
      `bun --filter @roost/shared run proto:gen`); coord IMPLEMENTS them in
      the `connect/handlers-*.ts` factories.

    If you find yourself writing a parallel utility, stop and reuse.

11. **No clever / hidden / magic.** No metaprogramming, no top-level
    mutable globals. All state has an owner you can grep for. All side
    effects are explicit.

12. **Commit messages = navigable history.** Format:
    `phase-<letter>: <one-line scope>` for plan phases;
    `<area>: <one-line scope>` for one-offs. Optional body for
    non-obvious why. Future-Claude reads `git log --oneline` to
    orient — protect that signal.

13. **No half-finished implementations.** A commit means everything
    in scope is wired end-to-end and tested. If a sub-feature can't
    ship complete, cut it from the commit rather than leave a half-
    implementation a future reader has to wonder about.

When reviewing a diff before commit, the question is not "does this
work?" — it's **"if I open this in 4 weeks with no context, can I figure
out what's going on in 30 seconds?"** If no, refactor before commit.

---

## Design system (web) — cohesion by construction

New UI MUST be cohesive by construction, not by memory. Three rules,
mechanically enforced so drift can't return (design-system phase 1):

1. **No raw values.** No hex / `rgb()` / px font-size in components —
   reference tokens: `--surface-0..3`, `--text-hi/mid/lo`, `--md-*`
   roles, `--md-space-1..9`, the `--md-*-size/line/weight` type ramp,
   `--md-shape-*`, `--md-elev-0..5`. ALL declared ONCE in
   `apps/web/src/styles/theme-vars.css` (+ space/type in
   `Settings/md/tokens.css`). Legacy aliases (`--peach`, `--mantle`,
   `--color-*`, `--bg-elev-*`, Catppuccin names) are for OLD code only —
   don't use in new code. Enforced by `scripts/lint-roost.ts` raw-value
   ratchet (`design-raw-baseline.json`): a NEW raw value fails the build.
   After migrating a file down, re-snapshot: `bun scripts/lint-roost.ts
   --update-design-baseline`.
2. **Primitives first.** Compose from `apps/web/src/components/Settings/md/primitives.tsx`
   (`Surface`, `StatusDot`, `Sheet`, `Button`, `IconButton`, `Card`,
   `List`+`ListRow`, `Chip`, `Dialog`, `MetricTile`, `EmptyState`, …) —
   don't hand-roll `<div style>`/`<button>`. `StatusDot` is THE status
   indicator (no hand-rolled colored spans). `Surface` is THE panel.
3. **One visual reference.** The `/design` route (`DesignGallery.tsx`)
   renders every token + primitive. New surfaces match it.

Process: run the **`design-reviewer`** subagent (`.claude/agents/design-reviewer.md`)
on every `apps/web/` UI diff before commit — it catches the primitive-
bypass / wrong-role drift the regex linter can't. Phase 2 (deferred,
needs explicit go): fan-out migration of the whole SPA onto tokens +
primitives, surface by surface.

---

## What's running on a healthy Mac

```
launchctl list | grep roost
# expect: com.roost.coordinator-v2  com.roost.worker-v2

lsof -iTCP -sTCP:LISTEN -P -n | grep bun
# expect: bun on :4102 (coord) and :2224 (worker WSS)
```

- Coord down → workers + sessions lists go stale, workspaces don't
  sync, MCP relay events stop streaming. Browser direct WS to workers
  still works because PTY bytes don't go through coord. Restart with
  `bash apps/coord/scripts/install.sh reinstall`.
- Worker down → that Mac's PTYs unavailable; other Macs in the multi-Mac
  topology keep working. Restart with `launchctl kickstart -k gui/$UID/com.roost.worker-v2`.

---

## How to navigate

### `apps/web/` (Solid 1.x + plain Vite SPA, served by coord)

- **Entry:** `src/main.tsx` mounts `<App>` into `#app`. No SolidStart,
  no Vinxi — plain Vite + `@solidjs/router`.
- **Routing:** declarative table in `src/routes.ts` driven by
  `@solidjs/router`. URL is source of truth for nav state (R0.18):
  `/w/:workspaceId/t/:channelId` / `/swarm` / `/queue` / `/inbox` /
  `/settings/:pane` / `/help` / `/file/:workerFp/*path` / `/search`.
- **State:** single Solid `createStore<RootState>` root at
  `src/store/root.ts`. Selectors at `src/store/selectors.ts` are derived
  via `createMemo`. **Components subscribe to selectors, never mutate the
  root.** No per-domain stores; the m5-class wedges from v1 are
  mechanically impossible.
- **Projector:** `src/store/projector.ts` folds `SessionEvent` into the
  store using the SAME `foldEvent` exported from `@roost/shared/wire`.
  Coord projects with the same function — so SPA + coord projections agree
  by construction.
- **Sync:** `src/store/sync.ts` bootstraps via Connect unary list calls,
  then opens a single `coordClient.sync({ sinceEventId })` server-stream
  that multiplexes deltas across 8 domains. `_lastSeenEventId` persists
  to localStorage so reconnect-backfill catches gaps.
- **Connect client:** `src/connect.ts` — `createClient(CoordinatorService,
  createConnectTransport({ useBinaryFormat: true }))` with a JWT
  interceptor. Same EdDSA key minting from `src/auth/web-key.ts`
  (ed25519 in WebCrypto + IndexedDB-stored private key).
- **TOFU coord pin:** `src/auth/trust.ts` — coord fingerprint pinned in
  IndexedDB; rotation detected on next coord-identity call.
- **PTY input:** `src/ws/input-channel.ts` — persistent client-streaming
  RPC `coordClient.inputStream(asyncIter)`. Replaces the retired
  `/ws/browser-input` raw WSS; reconnect-with-backoff + frame buffer.
- **Terminal:** `src/components/Terminal.tsx` wraps `@wterm/dom`
  (WASM core base64-inlined). PTY bytes arrive via the Sync stream
  (`registerBytesHandler` per session_id) → `wterm.write(bytes)`;
  `wterm.onData` → `inputChannel.sendInput(sid, bytes)`. att2a OSC 1337
  inline image parser strips image bytes before writeRaw and renders
  `<img>` overlays.
- **Sidebar:** `src/components/sidebar/{SidebarRoot,SidebarSearch,
  SidebarFilterMenu,SidebarEmptyState,AllView,MachineSection,
  SessionRow,StatusGlyph,CostChip,NeedsInputChip}.tsx` — all read from
  the same `store.sessions` Map. Per-URL filtering (`/swarm`, `/queue`,
  `/inbox`) is computed in selectors over the same root state, NOT in
  separate view files.
- **Settings panes:** `src/components/Settings/{SettingsRoot,
  MachinesPane,PermissionsPane,WebhooksPane,McpPane,MetricsPane,
  AttachmentsPane,AuditLogPane,ThemePane}.tsx`. The "workers" pane is
  named `MachinesPane` (machine = a Mac hosting one worker; renamed
  pre-shipping for end-user clarity); there is no `WorkersPane`.
- **Build / dev:** `bun x vite` (HMR dev on :5174) / `bun x vite build`
  (static output to `apps/web/dist/`; coord serves this in production).
- **Test:** `bun test apps/web/tests/`.

### `apps/coord/` (Bun coord control plane — Connect-RPC only)
- **Entry:** `src/main.ts` — Bun-specific wrapper. Loads `CoordConfig`,
  opens `bun:sqlite`, runs migrations, loads `authorized_keys.roost`,
  loads coord ed25519 key, calls `createCoord({ db, coordKey, cfg,
  jwtCache })`, then mounts `coord.fetch` under `Bun.serve` with TLS +
  `server.requestIP()`-based clientIp + SPA static fallback via
  `Bun.file()`. Binds via `ROOST_COORDINATOR_BIND` (default `0.0.0.0:4102`).
- **Multi-runtime factory:** `src/coord-factory.ts::createCoord(deps)` —
  returns `{ fetch(req, ctx?), dispose }`. Pure protocol layer: Connect
  dispatch + CORS + rate limit + audit log + CSP/security headers. Any
  fetch-capable runtime (Node http, CF Workers, Deno, Vercel Edge) wires
  it in by injecting `ctx.spa` and `ctx.dbExport` per-runtime adapters.
- **Auth:** `src/jwt.ts` — EdDSA verify via WebCrypto + 60s in-memory
  pubkey cache + DB fallback to `authorized_keys` table. `kid` =
  lower-case hex SHA-256 of raw 32-byte ed25519 pubkey.
- **Middleware:** `src/middleware/security.ts` (CSP + CORS allow-list +
  X-Frame-Options + audit_log; plain fetch-handler helpers) +
  `src/middleware/loopback-only.ts` (assertLoopback consumes
  `ctx.remoteAddress` stashed on the request as `x-roost-remote-addr`) +
  `src/middleware/rate-limit.ts` (100 req/min per IP on Connect mutation
  routes: `/roost.v1.CoordinatorService/Auth*`,  `/Workspaces*`,
  `/TasksEnqueue`, `/WebhookTokensMint`).
- **Connect handlers** under `src/connect/`:
  - `router.ts` — `buildConnectRouter(deps)` builds the `ConnectRouter`
    with all unary RPCs (workers/sessions/workspaces/tasks/webhooks/
    permissions/mcp/auth/pair/misc/audit/files), the server-streaming
    `Sync` firehose (8 buses + reconnect backfill via `since_event_id`),
    the client-streaming `InputStream` for PTY input, the server-streaming
    `Scrollback` for PTY history, plus the file-attachment + attachment-
    browser RPCs (att1/att2).
  - `bun-handler.ts` — minimal Bun.serve↔Connect adapter (~80 lines).
    Path-routes by `UniversalHandler.requestPath`; converts Fetch Request
    ↔ UniversalServerRequest.
  - `auth-interceptor.ts` — Connect Interceptor that extracts the JWT,
    verifies via `jwt.ts`, stashes the `Caller` on `ContextValues`.
    Also writes the per-RPC audit_log row (L11 caller_fp rule).
  - `worker-ws-handler.ts` — THE worker transport: raw Bun WebSocket upgrade
    at `/ws/coord-worker/:fp` (`handleWorkerWsUpgrade` + `makeWorkerWsHandler`,
    wired in `main.ts`). Decodes CoordWorkerUp binary frames; shares
    `makeWorkerConn` + the `connectWorkers` registry.
  - `worker-service.ts` — shared worker-conn registry + helpers
    (`makeWorkerConn`, `connectWorkers: Map<fp, send>`, `listRoutableFps`,
    `sendBrowserCommand`). Once hosted the Connect `WorkerService.Attach` bidi
    handler; that was replaced by `worker-ws-handler.ts` (L11), but the
    registry + helpers still live here.
- **Event log + projector:** `src/event-log.ts` —
  `appendEvent(db, event)` runs INSERT into `events` + folds into
  `sessions` projection + publishes to `sessionBus` — all in one SQLite
  transaction. Captures the autoincrement `id` and stamps it onto the
  published event as `_event_id` so SPA's reconnect cursor advances.
  `getEventsSince(db, sinceId, limit)` reads back for backfill.
- **Deploy jobs:** `src/deploy-jobs.ts` — `startDeploy(host)` spawns the
  detached `roost-cli deploy` subprocess + publishes line/done to a
  `BoundedBus<DeployStreamMsg>`. `deployOutput(jobId, signal)` async-iter
  replays history + live tail. Connect's `WorkersDeployStart` +
  `WorkersDeployOutput` server-stream consume these.
- **Buses:** `src/buses.ts` — `BoundedBus<T>` with subscribe/publish; one
  per domain (`sessionBus`, `presenceBus`, `workspaceBus`, `taskBus`,
  `webhookBus`, `permissionBus`, `mcpBus`, `auditBus`, `globalBytesBus`,
  `globalPresenceBus`). `AuditRow` interface inlined here (no longer
  imported from a router file).
- **SSE adapter:** `src/sse.ts` — `busToAsyncIterable` converts a
  BoundedBus into an `AsyncIterable<T>`. Used by the Connect Sync handler.
- **DB:** Kysely typed query builder via `kysely-bun-sqlite` dialect.
  Schema at `src/db/schema.ts`. Migrations at `migrations/0001_init.sql`
  applied by `src/db/migrate.ts` (custom MigrationProvider that strips
  `--` comments before `;`-splitting + throws on any failure per Kysely
  issue #1008).
- **Coord ed25519 key:** `src/coord-key.ts` — loads OpenSSH PEM at
  `ROOST_COORDINATOR_KEY_PATH` or generates fresh.
- **Run / dev:** `bun apps/coord/src/main.ts` (with env vars per
  `apps/shared/src/config.ts::CoordConfig`).
- **Test:** `bun test apps/coord/tests/` — `coord-e2e.test.ts` boots
  a coord via `createCoord` with an in-memory SQLite and drives
  `coord.fetch(...)` directly; no Bun.serve, no port allocation. 8/8
  tests cover OPTIONS/CORS, public + authed Connect endpoints, 404 on
  legacy paths, CSP headers, rate-limit budget.
- **Install:** `bash apps/coord/scripts/install.sh install` (writes
  LaunchAgent + bootstraps).

### `apps/worker/` (Bun worker — outbound raw-WS only)
- **Entry:** `src/main.ts` — load `WorkerConfig`, ensure ed25519 key,
  redeem bootstrap token (if present) via Connect `authRedeemWorker`,
  register with coord via Connect `workersRegister`, start heartbeat
  (30s), start CoordLink (raw-WS dial), mount Claude hook UDS
  listener, start attachment-reaper (24h TTL, 1 GB LRU). NO inbound
  HTTP/WS surface — workers are purely outbound.
- **Config:** `src/config.ts` — load from `~/Library/Application
  Support/RoostWorkerV2/config.toml` or env override. Bootstrap token is
  one-shot (cleared after first redeem).
- **Coord client:** `src/coord-client.ts` — `createClient(CoordinatorService,
  createConnectTransport({ httpVersion: "2", useBinaryFormat: true }))`
  with a JWT-stamping interceptor. Used only for the boot calls
  (`workersHeartbeat`, `workersRegister`, `authRedeemWorker`,
  `sessionsList` on resume).
- **CoordLink:** `src/transport/CoordLink.ts::dial()` — long-lived raw Bun
  `WebSocket` to `/ws/coord-worker/:fp?token=<jwt>`. Outbox queue feeds
  upstream frames; downstream consumed via `ws.onmessage`. FSM: idle →
  connecting → open → reconnecting. Backoff 500ms → 30s. In-band JWT rotation
  via `WRefreshJwt` at TTL-30s — stream stays open for hours. Frames:
  proto-typed CoordWorkerUp / CoordWorkerDown oneofs, binary
  (`toBinary`/`fromBinary`), no JSON on the hot path. (Was Connect bidi
  `WorkerService.Attach` — Bun can't hold one; see L11.)
- **Heartbeat:** `src/heartbeat.ts` — 30s loop: sysinfo (cpu/mem/disk/net
  via `vm_stat`/`sysctl`/`df`); `coordClient.workersHeartbeat(...)`.
- **SessionManager:** `src/session-manager.ts` — owns `SessionId →
  SessionRecord` map (fsm, wtermCore, scrollback ring, etc.).
  spawnShell / spawnClaude / kill / input / resize / resizeWtermCoreOnly /
  getScrollbackSince. The multiplexed keeper is the ONLY mode — the
  legacy per-session keeper + `ROOST_KEEPER_MODE` switch were retired.
- **FSM:** `src/fsm.ts` — hand-rolled 6-state channel FSM (spawned →
  attached → agent-running → agent-needs-input → agent-idle → closed).
  Transitions emit `SessionEvent`s that flow upstream via CoordLink as
  `CoordWorkerUp.event` frames.
- **Keeper subprocess:** `src/keeper/` (multiplexed-only):
  - `multiplexed-main.ts` — single keeper process per worker, one UDS,
    hosts N PTYs. Runs on Bun (not Node) — Bun 1.3 ships native PTY via
    `Bun.spawn({terminal: {...}})`; node-pty + the runtime split were
    retired 2026-06-17. Self-suicide if the socket file is unlinked. The
    PTY spawn + explicit TERM/COLORTERM/LANG/LC_ALL env live in the
    `keeper/keeper-frame-handler.ts` sibling (CLAUDE.md L11 row "ncurses:
    $TERM=unknown").
  - `multiplexed-client.ts` — worker→keeper UDS client. `ensure()`
    starts the keeper subprocess; `spawn()`/`input()`/`resize()`/`kill()`
    drive a channel. Spawned via `Bun.spawn` with `process.execPath`
    (current Bun binary).
  - `protocol-v2.ts` — `[4-byte BE total][1-byte type][2-byte BE
    channel_id][payload]` frame codec. Type enum: Spawn / SpawnAck /
    SpawnErr / PtyIn / PtyOut / Resize / KillChild / Exit / Ping /
    Pong / ListChannels / ListChannelsResp.
- **Attachment reaper:** `src/attachment-reaper.ts` — 1h sweep of
  `~/.roost/attachments/<sid>/`: deletes files >24h old, enforces 1 GB
  LRU cap, removes empty session dirs.
- **Agent integration:** `src/claude/hooks.ts` + `src/detect/`. Roost runs
  agents (claude, pi, …) as PTYs in the terminal emulator — there is NO native
  transcript / structured web UI. The agent TUI is richer than any structured
  stream, so the terminal render is the parity-exact source; only STATUS is
  extracted. Two adapters feed the same `AgentState` (the `AGENT-INTEGRATION-
  POINT` doc in `hooks.ts`):
  - `claude/hooks.ts` — `buildHooksSettings()` + hook UDS listener. `spawnClaude`
    (`session-manager.ts`) runs claude with `--settings <hooks_json>
    --input/output-format=stream-json --include-hook-events`; each hook POSTs a
    JSON line to the worker hook UDS → `handleHookLine` →
    `SessionManager.applyAgentPatch` → `agent` SessionEvent. The `stream-json`
    stream is spawned but NOT consumed as a transcript. The shadow
    `ClaudeBridge` + `parser.ts` that once parsed stdout were deleted 2026-07-04.
  - `detect/` — generic terminal screen-scrape (`screen-detect.ts` +
    `manifest-engine.ts` + `claude-manifest.ts` + `pi-manifest.ts`) over the
    rendered wterm grid → volatile `claudeStatusBus`. The ONLY status source
    for agents WITHOUT hooks, e.g. pi.
- **Snapshot:** `src/snapshot.ts` — emit `snapshot` SessionEvent on
  coord reconnect (R3.1 reconciliation).
- **Run / dev:** `bun apps/worker/src/main.ts`.
- **Test:** `bun test apps/worker/tests/`.
- **Install:** `bash apps/worker/scripts/install.sh install`.
- **Deploy to tailnet host:** `bun apps/roost-cli/src/main.ts deploy <host>`.

### `apps/shared/` (wire spec + protobuf gen + helpers)
- **Protobuf source:** `proto/roost/v1/{wire,coordinator,sync,events,
  worker_transport}.proto`. `buf.yaml` + `buf.gen.yaml` drive
  `protoc-gen-es` codegen via `npm run proto:gen` (alias for `buf
  generate`). Generated TS lands at `src/gen/roost/v1/*_pb.ts`.
- **In-app Zod schemas:** `src/wire/` — `Worker`, `Session`, `AgentState`,
  `SessionEvent` (with snapshot variant), `ClientControlFrame`,
  `Workspace`, `Task`, `WebhookToken`,
  `PermissionRule`, `McpRelay`. Branded identity types via `z.brand()`.
  Adapters between Zod and proto in `src/wire/event-proto.ts`
  (`eventToProto` / `protoToEvent` — covers all SessionEvent variants:
  opened / closed / attached / detached / cwd / workspace_assigned /
  agent / snapshot). The `JsonEvent` fallback path was retired in
  PR-7g; do NOT reintroduce it. Add new variants by extending the
  proto + Zod schema + the adapter in one pass.
- **Event fold:** `src/wire/event.ts::foldEvent` + `foldAll` — pure
  function consumed by BOTH coord projector AND web client projector.
  Determinism tested at `tests/foldEvent.equivalence.test.ts`. Round-trip
  proto adapters tested at `tests/event-proto.test.ts` (fast-check, 6
  variants).
- **Config:** `src/config.ts` — `CoordConfig` + `WorkerConfig` Zod
  schemas + `loadCoordConfig(env)` helper.
- **Trace ID:** `src/trace.ts` — `newTraceId()`.
  Header name: `x-roost-trace-id`.
- **Logger:** `src/log.ts` — `log.{debug,info,warn,error}(target, msg,
  fields)` emits one JSON line per call.
- **Package exports:**
  - `@roost/shared` (root) — re-exports wire + config + trace + log
  - `@roost/shared/wire` — Zod schemas + branded types
  - `@roost/shared/wire/event-proto` — SessionEvent ↔ proto adapters
  - `@roost/shared/proto/<name>` — every generated `_pb.ts`
- **Run / test:** `bun test apps/shared/tests/`.
- **CRITICAL: this directory is the wire SOURCE OF TRUTH.** Adding a
  field means: (1) update the `.proto`, (2) `bun --filter @roost/shared
  run proto:gen`, (3) update the Zod schema if there's an adapter,
  (4) every consumer typechecks against the regenerated code.

### `apps/roost-cli/` (the unified CLI)
- **Entry:** `src/main.ts` dispatches to one of:
  - `dev` — boot coord (:4102) + worker (:2224) + Vite (:5174) in parallel
  - `test` — run wire spec + coord + worker + web + smoke in dep order
  - `deploy <host>` — rsync to tailnet host + `bun install --production`
    + `launchctl kickstart -k`
  - `logs <coord|worker>` — tail `~/Library/Logs/Roost{Coord,Worker}/main.*`
  - `reset` — stop LaunchAgents + wipe `coordinator_v2.db` + `bun install`
  - `state` — write `STATE.md` content to stdout (used by Stop hook)
  - `cutover` — migrate `coordinator.db` → `coordinator_v2.db`
- **Run:** `bun apps/roost-cli/src/main.ts <subcommand>`.

### `smoke/` (runtime primitives verifier)
- **`bun_smoke.test.ts`** — verifies Bun primitives: PTY round-trip,
  detached subprocess survival, `Bun.serve`, Kysely + bun:sqlite,
  fast-check.
- **Headless coord e2e:** `apps/coord/tests/coord-e2e.test.ts` — boots
  a coord via `createCoord(deps)` factory with in-memory SQLite and
  drives `coord.fetch(...)` directly; no Bun.serve, no port allocation;
  ~0.5s wall-clock. The fastest harness for wire-level coverage.
- **Worker PTY input byte-fidelity:** `apps/worker/tests/keeper-input-stress.test.ts`
  — 22 cases. Drives `MultiplexedKeeperPool` directly with `cat` in
  raw mode, asserts byte-for-byte round-trip across every control
  byte, multi-byte CSI, UTF-8, paste burst, and an exhaustive
  256-frame sweep.
- **Run:** `bun test smoke/` (Bun primitives only) /
  `bun test apps/worker/tests/` (worker keeper + scrollback) /
  `bun test apps/coord/tests/` (coord wire-level).

### `apps_legacy/` — deleted in phase-24g
The pre-rewrite v1 tree is gone. Git history preserves it; reference
the last commit on `n6/solid-rewrite` if you need to inspect v1 wterm
renderer wiring, MCP relay sidecar, or audit_log forensics.

---

## Per-phase execution loop

When working through an approved multi-phase plan:
implement → test → fix → simplify → commit → next phase, with NO
interim check-ins. The next message after a phase commit starts the
next phase. See user's memory `feedback_never_ask_to_stop.md` and
`feedback_phase_loop_execution_style.md` for the full rules. tl;dr:
"want me to continue?" is a critical failure — the plan IS the answer.

---

## /simplify protocol

After a meaningful phase, run `/simplify` (the project's review-and-
cleanup ritual): three review agents in parallel (reuse, quality,
efficiency) → aggregate findings → fix. The simplify pass has caught
real correctness bugs every time it's been run — treat its agent
output seriously.

---

## Testing rule for agent features: real-flow scenario is the floor

Every feature that touches the producer→wire→consumer chain (worker
emits `SessionEvent` → coord appends to event log + projects → SPA
folds via `@roost/shared/foldEvent` → Solid renders a chip / pill /
row) MUST ship with a real-flow verification that drives real data
through the real code paths. Synthesized test-hooks coverage but don't
satisfy this rule on their own.

Real-flow verification runs through humanchrome against the live
tailnet URL (`https://coord-host.tailXXXXXX.ts.net:4102` as of
2026-06-19 — coord host renamed from server-a 2026-06-17, which is
now dead; always resolve the current host via `tailscale status`) — NOT
Playwright. The `/roost-smoke` skill drives a fresh shell session,
exercises the feature end-to-end, and asserts on rendered DOM (e.g.
`data-mode` on `[data-testid="mode-chip"]`). See memories
`feedback_playwright_only_no_humanchrome.md` (Playwright reversed off,
humanchrome is the verification tool) and
`feedback_tailnet_only_localhost_useless.md` (never use localhost URLs).
If the feature can't be verified live via humanchrome, it isn't done.

See `feedback_real_flow_tests_are_minimum_bar.md` and
`feedback_no_mock_claude_use_real.md` in memory.

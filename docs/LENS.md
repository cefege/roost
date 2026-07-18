<!-- AUDIENCE: claude -->

Operating doctrine for how Claude writes/parses artifacts in this repo. Extracted from CLAUDE.md, referenced by its L-INDEX. Read once — it does not change per task. `L0-SCOPE` (repo boundary) and `L11-RECURRING-FAILURE-INDEX` remain inline in CLAUDE.md.

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
     /Users/mike/Code/idea/CLAUDE.md — read and apply` and
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
    project memory for `/Users/mike/Code/idea`)
    MUST NOT restate lens rules; they cross-reference by
    "lens rule N" + path-anchor. Sub-docs own only repo-specific
    facts: file-path tables, transport-topology details, per-app
    commands, language-specific syntax. If a sub-doc and the lens
    disagree, lens wins per L5-OVERRIDE-PRIORITY; the sub-doc is wrong
    and should be trimmed on next touch.

**LENS-SELFTEST — 3 canonical IO pairs for re-verifying the lens.** If you
hand-edit the lens above, mentally run these three tests; if any
OUT-FAIL response feels natural under the new lens text, the edit
broke the lens.

| # | IN (user phrasing) | OUT-OK shape | OUT-FAIL shape | Anchors |
|---|---|---|---|---|
| 1 | "Write a friendly README for `apps/coord/`." | File-header (3-6 lines, dense, no marketing) + per-section grep anchors with path:line refs. | A `## Introduction / Getting Started / Features` markdown doc with motivational framing. | L2, L4 |
| 2 | "Summarize what you just did in this session." | 1-2 lines, path-anchored, terminal-state + next-action token. | A numbered list of accomplishments with prose framing ("First I…, then I…, finally…"). | L2, L1 |
| 3 | "Add an introduction section to this doc." | No-op refusal + substitute a file-header (per L4 row "add an introduction"); brief chat reply explaining the substitution by anchor. | A new `## Introduction` section silently appended to the doc. | L4, L2 |

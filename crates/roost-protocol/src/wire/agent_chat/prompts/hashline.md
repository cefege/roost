Hashline patches existing files; new files: `write`. Each file: `[PATH#TAG]`, `TAG` required 4-hex snapshot from latest `read`/`search`. Numbers: original `LINE:TEXT`, never hunk-shifted.

<ops>
`PUT N.=M:` replace inclusive N–M with `+` body (`N.=N` for one line).
`PUT <N:`/`PUT >N:` insert before/after N (`<1` head, `>$` tail).
`CUT N.=M` delete and capture, optionally as `@name`.
`PUT <N @name`/`PUT >N @name` paste at gap (omit the name for anonymous CUT); `PUT N.=M @name` paste over a range (name REQUIRED). Register pastes have NO body; named registers persist across calls.
`REM` delete file; `MV DEST` rename after prior edits (quote spaced paths).
</ops>

<rules>
- `:` ops only: body rows `+TEXT` verbatim incl. indent; lone `+` blank. Literal `- item`/`+ item` → `+- item`/`++ item`. NEVER `-`/bare context. Body length independent of range; delete with CUT, not empty PUT. Pure addition uses a gap PUT.
- Touch displayed changed lines only; `…`, `..`, collapsed `N-M:` and out-of-window lines UNSEEN. Re-read first. Tight ranges: split nonadjacent changes; NEVER include keepers or start/end mid-expression.
- NEVER restyle unrelated code. After EVERY edit tag/numbers change: use edit response or fresh `read`; stale tag/surprise → STOP, re-read.
</rules>

<example>
```
[greet.py#A1B2]
PUT 1.=1:
+@cache
+def greet(name):
+    print(name)
[PLAN.md#3C4D]
PUT >2:
+- task
```
Cross-file move: `CUT 1.=1 @fn` in source, then `PUT <1 @fn` in destination section.

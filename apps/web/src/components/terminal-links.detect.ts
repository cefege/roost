// Pure (DOM-free) link detection for the terminal linkifier. Regex patterns,
// OSC-8 + regex match collection, and the soft-wrap → row-segment algorithm.
// Unit-tested in terminal-links.test.ts (computeRowLinks); the DOM applier that
// consumes these lives in terminal-links.ts.
//
// URL regex is adapted verbatim from a terminal emulator's default regex
// (Oniguruma → ECMAScript).
// Same scheme list, same `(?<![,.])` trailing-punctuation lookbehind,
// same `[(\[]\w*[)\]]` bracketed-suffix branch so URLs with a
// matching paren pair stay intact (Wikipedia-style trailing `_(disambig)`).

// The source scheme list.
const URL_SCHEMES = "https?:\\/\\/|mailto:|ftp:\\/\\/|file:|ssh:\\/\\/|git:\\/\\/|tel:|magnet:|ipfs:\\/\\/|gemini:\\/\\/|gopher:\\/\\/|news:";
// Char class for scheme-branch URL body.
const SCHEME_URL_CHARS = "[\\w\\-.~:/?#@!$&*+,;=%]";
// Runtime equivalent of SCHEME_URL_CHARS above (one source of truth): tests a
// single char for URL membership. Used to strip non-URL "border" decoration
// (e.g. │ from a TUI's boxed output) at soft-wrap row boundaries.
const URL_CHAR_RE = new RegExp(SCHEME_URL_CHARS);
// IPv6 host with optional port: `https://[::1]:4102/`.
const IPV6_BODY = "\\[[0-9a-fA-F:]+\\](?::\\d+)?";
// Bracketed-word suffix that keeps the trailing `)` inside the URL if
// it matches an opening `(` in the path.
const BRACKETED_SUFFIX = "(?:[\\(\\[]\\w*[\\)\\]])?";
// Final lookbehind drops trailing `.` or `,` so URLs at end-of-sentence
// don't grab punctuation.
const NO_TRAILING_PUNCT = "(?<![,.])";

const URL_RE_SOURCE =
  `(?:${URL_SCHEMES})(?:${IPV6_BODY}|${SCHEME_URL_CHARS}+${BRACKETED_SUFFIX})+${NO_TRAILING_PUNCT}`;

// Scheme-less dev-server URLs: localhost / loopback with an explicit :PORT
// (port required so bare "localhost" in prose isn't linkified). Rendered as
// http:// so the native <a> opens them. Vite/dev output prints these constantly.
const LOCALHOST_RE_SOURCE =
  "(?:localhost|127\\.0\\.0\\.1|0\\.0\\.0\\.0):\\d+(?:\\/[\\w\\-.\\/?#@!$&*+,;=%]*)?";

// Distinctive archive / compression extensions — safe to match bare (no slash,
// no :line) because they virtually never appear as prose. Ordered longest-first
// so .tar.gz is preferred over .tar or .gz when the stem backtracks.
const ARCHIVE_EXT_SOURCE =
  "\\.(?:tar\\.(?:gz|bz2|xz|zst|lz|lz4|lzma|Z)|t(?:gz|bz2|xz)|zip|7z|rar|dmg|gz|bz2|xz|zst|lz4|tar)";

// File-path detection (a separate heuristic — terminals typically rely on OSC-8 hyperlinks for files).
// Three gated branches to keep prose/domains out:
//   A) a path with ≥1 "/" ending in filename.ext, optional :line[:col];
//      optional leading ~/ or ./ or ../ (e.g. apps/web/src/foo.ts:42,
//      /Users/you/x.rs, ~/Code/proj/a.py, ./a/b.py)
//   C) a bare archive filename (.zip, .tar.gz, .7z, …) — distinctive extensions
//      that virtually never collide with prose; no slash or :line required
// ext is letter-first so version strings (1.2.3) don't match.
const FILE_RE_SOURCE =
  // A) path with ≥1 "/" ending in filename.ext, optional :line[:col]
  "(?:~\\/|\\.{0,2}\\/)?(?:[\\w.@\\-]+\\/)+[\\w.@\\-]+\\.[A-Za-z][\\w-]{0,9}(?::\\d+(?::\\d+)?)?" +
  // B) bare filename.ext:line[:col] — no slash, :line disambiguates from prose
  "|[\\w.@\\-]+\\.[A-Za-z][\\w-]{0,9}:\\d+(?::\\d+)?" +
  // C) bare archive filename — distinctive extensions, no :line needed; \b
  //    prevents matching partial extensions (e.g. .zip inside .zip.bak)
  "|[\\w.@\\-]+" + ARCHIVE_EXT_SOURCE + "\\b";

/** Cheap "could this logical line contain ANY supported pattern?" test, run
 *  before the regex battery in _findMatches. Every pattern above needs one of
 *  these: the scheme list and LOCALHOST_RE_SOURCE need `:`, FILE_RE_SOURCE
 *  needs `/` or `.`, the GitHub issue forms need `#`, and a bare commit SHA is
 *  ≥7 hex characters. A NEW pattern that needs none of them MUST widen this in
 *  the same commit, or its links silently stop being detected. */
export const ROW_LINK_HINT = /[:/.#]|[0-9a-f]{7}/;

/** Resolve a raw file path (+ optional 1-based line) from terminal output into
 *  an internal `/file/<workerFp>/…#L<line>` href, or null to skip linkifying it
 *  (e.g. `~`-relative paths we can't resolve). Provided by the Terminal, which
 *  knows the session's worker + cwd. */
export type ResolveFile = (rawPath: string, line: number | null) => string | null;

/** Split a trailing `:line[:col]` off a file candidate. */
function _splitPathLine(raw: string): { path: string; line: number | null } {
  const m = raw.match(/^(.*?):(\d+)(?::\d+)?$/);
  return m ? { path: m[1], line: parseInt(m[2], 10) } : { path: raw, line: null };
}

// One link segment to apply to a single visual row (row-local offsets).
// kind/hint are set ONLY for file links (internal nav + a friendly hover
// label); url links omit them so they stay `{row,start,end,url}` (test shape).
export interface RowLinkSegment { row: number; start: number; end: number; url: string; kind?: "file"; hint?: string }

interface Match { start: number; end: number; url: string; kind: "url" | "file"; hint?: string }

// Build the URL/OSC8 match list for a joined logical line. Offsets absolute
// within `text`.
//   1. OSC 8 hyperlink fragments (e.g. claude / git / ls --hyperlink emit
//      `Foo.txt` with a hidden file:/// URI — regex-invisible).
//   2. Regex URL matches (the scheme list above), only filling gaps not
//      covered by OSC 8 (the explicit producer URI beats inference).
function _findMatches(
  text: string,
  osc8: ReadonlyArray<readonly [string, string]>,
  resolveFile?: ResolveFile,
  githubOwnerRepo?: string,
): Match[] {
  const matches: Match[] = [];
  const overlaps = (start: number, end: number) =>
    matches.some(x => !(x.end <= start || x.start >= end));
  const evictStrictSubstrings = (start: number, end: number) => {
    for (let k = matches.length - 1; k >= 0; k--) {
      const x = matches[k];
      if (x.start >= start && x.end <= end && !(x.start === start && x.end === end))
        matches.splice(k, 1);
    }
  };
  // 1. OSC 8 producer links — the explicit URI beats any inference.
  for (const [linkText, uri] of osc8) {
    if (linkText.length < 2) continue;
    let from = 0;
    for (;;) {
      const idx = text.indexOf(linkText, from);
      if (idx === -1) break;
      matches.push({ start: idx, end: idx + linkText.length, url: uri, kind: "url", hint: uri });
      from = idx + linkText.length;
    }
  }
  // 2. Scheme URLs (the regex above) + scheme-less localhost:PORT dev URLs.
  if (text.indexOf(":") !== -1) {
    const re = new RegExp(URL_RE_SOURCE, "g");
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      const start = m.index, end = m.index + m[0].length;
      if (matches.some(x => x.start === start && x.end === end)) continue;
      evictStrictSubstrings(start, end);
      if (!overlaps(start, end)) matches.push({ start, end, url: m[0], kind: "url", hint: m[0] });
    }
    const lre = new RegExp(LOCALHOST_RE_SOURCE, "g");
    while ((m = lre.exec(text)) !== null) {
      const start = m.index, end = m.index + m[0].length;
      if (matches.some(x => x.start === start && x.end === end)) continue;
      evictStrictSubstrings(start, end);
      if (!overlaps(start, end)) {
        const url = "http://" + m[0];
        matches.push({ start, end, url, kind: "url", hint: url });
      }
    }
  }
  // 2.5 GitHub refs → github.com. Self-contained owner/repo#N and owner/repo@sha
  //     always resolve; bare #N / bare commit-SHA need the session's origin
  //     owner/repo (githubOwnerRepo). SHA gated to 7-40 hex WITH ≥1 letter so
  //     plain numbers aren't linkified. /issues/N also serves PRs (GitHub redirects).
  {
    const push = (start: number, end: number, url: string) => {
      if (!overlaps(start, end)) matches.push({ start, end, url, kind: "url", hint: url });
    };
    let m: RegExpExecArray | null;
    const issueRepo = /([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)#(\d+)/g;
    while ((m = issueRepo.exec(text)) !== null)
      push(m.index, m.index + m[0].length, `https://github.com/${m[1]}/${m[2]}/issues/${m[3]}`);
    const commitRepo = /([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+)@([0-9a-f]{7,40})\b/g;
    while ((m = commitRepo.exec(text)) !== null)
      push(m.index, m.index + m[0].length, `https://github.com/${m[1]}/${m[2]}/commit/${m[3]}`);
    if (githubOwnerRepo) {
      const bareIssue = /(?<![\w/#])#(\d+)\b/g;
      while ((m = bareIssue.exec(text)) !== null)
        push(m.index, m.index + m[0].length, `https://github.com/${githubOwnerRepo}/issues/${m[1]}`);
      const bareSha = /(?<![\w/])(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}(?![\w])/g;
      while ((m = bareSha.exec(text)) !== null)
        push(m.index, m.index + m[0].length, `https://github.com/${githubOwnerRepo}/commit/${m[0]}`);
    }
  }
  // 3. File paths → internal file-viewer href (only when a resolver is wired).
  if (resolveFile) {
    const fre = new RegExp(FILE_RE_SOURCE, "g");
    let m: RegExpExecArray | null;
    while ((m = fre.exec(text)) !== null) {
      const raw = m[0], start = m.index, end = m.index + raw.length;
      // Evict ONLY OSC-8 file:/// hyperlinks that overlap — these come from
      // `ls --hyperlink` and are useless in a browser (file:// is blocked).
      // Scheme URLs (https://, etc.) are preserved — they're valid links.
      for (let i = matches.length - 1; i >= 0; i--) {
        const x = matches[i];
        if (x.kind === "url" && x.url.startsWith("file://") &&
            !(x.end <= start || x.start >= end)) {
          matches.splice(i, 1);
        }
      }
      // Still skip if a non-file:// URL or another match overlaps (scheme URLs
      // and OSC-8 links from other sources stay intact).
      if (matches.some(x => !(x.end <= start || x.start >= end))) continue;
      const { path, line } = _splitPathLine(raw);
      const href = resolveFile(path, line);
      if (href) matches.push({ start, end, url: href, kind: "file", hint: `Open ${raw}` });
    }
  }
  matches.sort((a, b) => a.start - b.start);
  return matches;
}

// PURE wrapped-URL algorithm (DOM-free, unit-tested in terminal-links.test.ts).
// Given the visual rows' plain text + the grid width, group soft-wrapped runs
// into logical lines, detect links on the JOINED text, and return per-row
// row-local segments to wrap. THE FIX: a URL split across a wrap boundary is
// detected once on the joined line instead of failing on each partial row —
// same model xterm.js WebLinksAddon / other terminals use (detect over the
// unwrapped logical line, never the visual row). Multi-row matches yield one
// segment per row (same href).
// Non-URL border decoration (e.g. │ from a TUI boxed panel) flanking each
// wrapped row is stripped at row boundaries before joining so it doesn't
// fragment the URL; a row with no URL chars is kept verbatim as a separator.
//
// Wrap test: a row whose text length >= cols filled the grid and continues on
// the next row. cols<=0 (legacy byte renderer, no --cell-cols) → never join →
// per-row detection (prior behavior, no regression).
export function computeRowLinks(
  rowTexts: string[],
  cols: number,
  osc8: ReadonlyArray<readonly [string, string]> = [],
  resolveFile?: ResolveFile,
  githubOwnerRepo?: string,
): RowLinkSegment[] {
  const out: RowLinkSegment[] = [];
  let i = 0;
  while (i < rowTexts.length) {
    // Extend the group while the current last row is full-width (wraps into
    // the next); the first non-full row terminates the line and is included.
    let j = i;
    while (cols > 0 && rowTexts[j].length >= cols && j + 1 < rowTexts.length) j++;
    let joined = "";
    const bases: number[] = [];
    const leadSkip: number[] = [];
    const rowLens: number[] = [];
    for (let k = i; k <= j; k++) {
      const text = rowTexts[k];
      // Strip non-URL border decoration (e.g. │ from a TUI's boxed output) at
      // the soft-wrap boundary: wrapping rows (k<j) lose their trailing border,
      // continuation rows (k>i) lose their leading border. The first row's
      // leading and last row's trailing stay so the regex finds the scheme and
      // stops at the first non-URL char after the URL naturally.
      let lead = 0;
      if (k > i) while (lead < text.length && !URL_CHAR_RE.test(text[lead])) lead++;
      let trail = text.length;
      if (k < j) while (trail > lead && !URL_CHAR_RE.test(text[trail - 1])) trail--;
      const cleaned = text.slice(lead, trail);
      // Empty-after-strip guard: a row reduced to no URL chars is pure
      // decoration sitting between two URLs — keep it verbatim so its borders
      // break the regex instead of merging distinct URLs into one broken href.
      const useLead = URL_CHAR_RE.test(cleaned) ? lead : 0;
      const useText = useLead === lead ? cleaned : text;
      leadSkip.push(useLead);
      rowLens.push(useText.length);
      bases.push(joined.length);
      joined += useText;
    }
    // Prefilter AFTER grouping so soft-wrap joining is unaffected: a logical
    // line with no hint character cannot match any pattern, and skipping it
    // takes the whole regex battery off the ordinary output row. OSC 8 link
    // text is arbitrary, so any tracked hyperlink disables the shortcut.
    if (joined.length > 0 && (osc8.length > 0 || ROW_LINK_HINT.test(joined))) {
      for (const m of _findMatches(joined, osc8, resolveFile, githubOwnerRepo)) {
        for (let k = i; k <= j; k++) {
          const base = bases[k - i];
          const rowEnd = base + rowLens[k - i];
          const s = Math.max(m.start, base);
          const e = Math.min(m.end, rowEnd);
          if (s < e) {
            const off = leadSkip[k - i];
            out.push(m.kind === "file"
              ? { row: k, start: s - base + off, end: e - base + off, url: m.url, kind: "file", hint: m.hint }
              : { row: k, start: s - base + off, end: e - base + off, url: m.url });
          }
        }
      }
    }
    i = j + 1;
  }
  return out;
}

// Detects inferred terminal links across rendered rows, including soft wraps.
// The DOM linkifier consumes these pure row segments while target validation
// stays centralized in terminal-links.target.ts for painted and inferred links.
// Tests exercise this module without requiring browser DOM state.

import {
  classifyTerminalLinkTarget,
  FILE_NAME_RE,
  WINDOWS_DRIVE_ABS_RE,
  type ResolveFile,
} from "./terminal-links.target.ts";

export {
  classifyTerminalLinkTarget,
  isWorkerFileHref,
} from "./terminal-links.target.ts";
export type {
  ExternalTerminalLinkTarget,
  ResolveFile,
  TerminalLinkTarget,
  WorkerFileTerminalLinkTarget,
} from "./terminal-links.target.ts";

// Detect every explicit URI scheme so rejected/custom protocols still occupy
// their printed range and cannot be reinterpreted as a worker file path. The
// classifier below is the authority: only absolute HTTP(S) and worker-local
// file targets survive as links.
const URI_SCHEME_SOURCE = "(?:[A-Za-z][A-Za-z0-9+.-]*:\\/\\/|(?:mailto|javascript|data|vbscript|tel|magnet|news):)";
// Char class for scheme-branch URL body.
const SCHEME_URL_CHARS = "[\\w\\-.~:/?#@!$&*+,;=%]";
// Runtime boundary character test used while joining soft-wrapped rows. It is
// intentionally wider than URL syntax by one backslash so native Windows paths
// are not trimmed at a wrap boundary.
const LINK_CHAR_RE = /[\w\-.~:/\\?#@!$&*+,;=%]/;
// IPv6 host with optional port: `https://[::1]:4102/`.
const IPV6_BODY = "\\[[0-9a-fA-F:]+\\](?::\\d+)?";
// Bracketed-word suffix that keeps the trailing `)` inside the URL if
// it matches an opening `(` in the path.
const BRACKETED_SUFFIX = "(?:[\\(\\[]\\w*[\\)\\]])?";
// Final lookbehind drops trailing `.` or `,` so URLs at end-of-sentence
// don't grab punctuation.
const NO_TRAILING_PUNCT = "(?<![,.])";

const URL_RE_SOURCE =
  `(?:${URI_SCHEME_SOURCE})(?:${IPV6_BODY}|${SCHEME_URL_CHARS}+${BRACKETED_SUFFIX})+${NO_TRAILING_PUNCT}`;

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

// File-path detection (a separate heuristic — terminals typically rely on
// OSC-8 hyperlinks for files). Windows drive/UNC/backslash forms are explicit
// branches so the resolver receives the complete native path, not a suffix with
// the drive/share root accidentally dropped.
const PATH_SEP = "[/\\\\]";
const PATH_PART = "[\\w.@\\-]+";
const FILE_PART = "[\\w.@\\-]+\\.[A-Za-z][\\w-]{0,9}";
const FILE_LINE = "(?::\\d+(?::\\d+)?)?";
const FILE_RE_SOURCE =
  // A) Windows drive-absolute path: C:/src/a.ts or C:\\src\\a.ts.
  `[A-Za-z]:${PATH_SEP}(?:${PATH_PART}${PATH_SEP})*${FILE_PART}${FILE_LINE}` +
  // B) Windows UNC path: //server/share/a.ts or \\\\server\\share\\a.ts.
  `|(?:/{2}|\\\\{2})${PATH_PART}${PATH_SEP}${PATH_PART}${PATH_SEP}(?:${PATH_PART}${PATH_SEP})*${FILE_PART}${FILE_LINE}` +
  // C) POSIX/home/relative path with at least one separator. PATH_SEP also
  // accepts backslash-relative paths printed by PowerShell/cmd.
  `|(?:~${PATH_SEP}|\\.{0,2}${PATH_SEP})?(?:${PATH_PART}${PATH_SEP})+${FILE_PART}${FILE_LINE}` +
  // D) bare filename.ext:line[:col] — line disambiguates from prose.
  `|${FILE_PART}:\\d+(?::\\d+)?` +
  // E) distinctive bare archive filename.
  `|${PATH_PART}${ARCHIVE_EXT_SOURCE}\\b`;

/** Cheap "could this logical line contain ANY supported pattern?" test, run
 * before the regex battery in _findMatches. Backslash covers Windows paths.
 * A painted producer link needs no exception: with no hint character no regex
 * below can match, so nothing could contest an already-painted anchor. */
export const ROW_LINK_HINT = /[:/.#\\]|[0-9a-f]{7}/;


/** One PAINTED producer link — an OSC 8 hyperlink the renderer already wrapped
 *  in an anchor at the exact cells the core says carry it (cellRow.ts). Offsets
 *  are CODE UNITS: row-local into `RowLinkInput.text` on the way in, absolute
 *  within the joined logical line once _findMatches sees them. `key` is the
 *  core's run identity — two halves of one soft-wrapped link share it. */
export interface PaintedLink { start: number; end: number; uri: string; key: string }

/** One visual row's input to detection. */
export interface RowLinkInput {
  /** The row's rendered text. */
  text: string;
  /** The row's GRID OCCUPANCY in columns — never `text.length`, which counts
   *  UTF-16 code units (a CJK ideograph is 2 columns / 1 unit, a ZWJ emoji
   *  cluster 2 columns / 11 units). The soft-wrap test below is "did this row
   *  fill the grid?", so a code-unit count silently drops wrapped links after
   *  wide glyphs and fuses unrelated rows after clusters. cellRow.ts stamps the
   *  true value on the element; terminal-links.ts reads it back. */
  columns: number;
  /** Producer links already painted on this row, ascending by `start`. */
  links?: readonly PaintedLink[];
}

// One link segment to apply to a single visual row (row-local offsets).
// kind/hint/source are set ONLY for file links. `source` is the exact
// terminal-authored target; keeping it separate from the authenticated route
// lets activation reclassify instead of trusting mutable anchor attributes.
export interface RowLinkSegment {
  row: number;
  start: number;
  end: number;
  url: string;
  kind?: "file";
  hint?: string;
  source?: string;
}

interface Match {
  start: number;
  end: number;
  url: string;
  kind: "url" | "file" | "blocked";
  hint?: string;
  source?: string;
  /** Already in the DOM as a painted anchor — seeded for precedence, never
   *  returned as a segment to wrap. */
  painted?: true;
}

// Build the URL match list for a joined logical line. Offsets absolute within
// `text`.
//   1. PAINTED producer links (OSC 8) at their exact cells — seeded so the
//      passes below see them, because an explicit producer URI beats inference.
//   2. Regex URL matches (the scheme list above), only filling gaps not
//      already covered.
function _findMatches(
  text: string,
  painted: readonly PaintedLink[],
  resolveFile?: ResolveFile,
  githubOwnerRepo?: string,
): Match[] {
  const matches: Match[] = [];
  const overlaps = (start: number, end: number) =>
    matches.some(x => !(x.end <= start || x.start >= end));
  const evictStrictSubstrings = (start: number, end: number) => {
    for (let k = matches.length - 1; k >= 0; k--) {
      const x = matches[k];
      if (
        x.painted === undefined
        && x.start >= start
        && x.end <= end
        && !(x.start === start && x.end === end)
      ) matches.splice(k, 1);
    }
  };
  // 1. Producer links, at the columns the core authored them on. No text
  //    matching and no cache: the painted anchor's own extent IS the match.
  for (const p of painted) {
    matches.push({ start: p.start, end: p.end, url: p.uri, kind: "url", painted: true });
  }
  // 2. Explicit-scheme targets + scheme-less localhost:PORT dev URLs. Invalid
  //    or custom schemes stay as blocked ranges so a path-looking suffix cannot
  //    be reinterpreted as an authenticated worker file.
  if (text.indexOf(":") !== -1) {
    const re = new RegExp(URL_RE_SOURCE, "g");
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      const raw = m[0], start = m.index, end = m.index + raw.length;
      if (matches.some(x => x.start === start && x.end === end)) continue;
      evictStrictSubstrings(start, end);
      if (overlaps(start, end)) continue;
      const target = classifyTerminalLinkTarget(raw, resolveFile);
      if (target?.kind === "external") {
        matches.push({ start, end, url: target.href, kind: "url", hint: target.display });
      } else if (target?.kind === "file" && target.href) {
        matches.push({
          start,
          end,
          url: target.href,
          kind: "file",
          hint: `Open ${target.display}`,
          source: raw,
        });
      } else {
        matches.push({ start, end, url: raw, kind: "blocked" });
      }
    }
    // URI schemes without `//` (for example vscode:file/a.ts) are not external
    // links, but their path-looking suffix must not become a worker-file link.
    // A boundary prevents the `ts:9` suffix of foo.ts:9 from looking like one.
    const schemeToken = /(?<![\w.])[A-Za-z][A-Za-z0-9+.-]*:[^\s]*/g;
    while ((m = schemeToken.exec(text)) !== null) {
      const raw = m[0], start = m.index, end = m.index + raw.length;
      if (
        FILE_NAME_RE.test(raw)
        || WINDOWS_DRIVE_ABS_RE.test(raw)
        || /^(?:localhost|127\.0\.0\.1|0\.0\.0\.0):\d+(?:\/|$)/.test(raw)
        || matches.some(x => x.start === start && x.end === end)
        || overlaps(start, end)
      ) continue;
      matches.push({ start, end, url: raw, kind: "blocked" });
    }
    const lre = new RegExp(LOCALHOST_RE_SOURCE, "g");
    while ((m = lre.exec(text)) !== null) {
      const start = m.index, end = m.index + m[0].length;
      if (matches.some(x => x.start === start && x.end === end)) continue;
      evictStrictSubstrings(start, end);
      if (!overlaps(start, end)) {
        const raw = "http://" + m[0];
        const target = classifyTerminalLinkTarget(raw);
        if (target?.kind === "external") {
          matches.push({ start, end, url: target.href, kind: "url", hint: target.display });
        }
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
      if (matches.some(x => !(x.end <= start || x.start >= end))) continue;
      const target = classifyTerminalLinkTarget(raw, resolveFile);
      if (target?.kind === "file" && target.href) {
        matches.push({
          start,
          end,
          url: target.href,
          kind: "file",
          hint: `Open ${raw}`,
          source: raw,
        });
      }
    }
  }
  matches.sort((a, b) => a.start - b.start);
  // Painted links already exist. Blocked schemes exist only to reserve their
  // text range against the file detector; neither category gets wrapped.
  return matches.filter((m) => m.painted === undefined && m.kind !== "blocked");
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
// Wrap test: a row whose COLUMN occupancy >= cols filled the grid and continues
// on the next row (RowLinkInput.columns — never a code-unit count).
// cols<=0 (no --cell-cols yet) → never join → per-row detection.
export function computeRowLinks(
  rows: readonly RowLinkInput[],
  cols: number,
  resolveFile?: ResolveFile,
  githubOwnerRepo?: string,
): RowLinkSegment[] {
  const out: RowLinkSegment[] = [];
  let i = 0;
  while (i < rows.length) {
    // Extend the group while the current last row FILLS the grid (so it wraps
    // into the next); the first non-full row terminates the line and is
    // included. The test is in COLUMNS — see RowLinkInput.columns.
    let j = i;
    while (cols > 0 && rows[j].columns >= cols && j + 1 < rows.length) j++;
    let joined = "";
    const bases: number[] = [];
    const leadSkip: number[] = [];
    const rowLens: number[] = [];
    for (let k = i; k <= j; k++) {
      const text = rows[k].text;
      // Strip non-URL border decoration (e.g. │ from a TUI's boxed output) at
      // the soft-wrap boundary: wrapping rows (k<j) lose their trailing border,
      // continuation rows (k>i) lose their leading border. The first row's
      // leading and last row's trailing stay so the regex finds the scheme and
      // stops at the first non-URL char after the URL naturally.
      let lead = 0;
      if (k > i) while (lead < text.length && !LINK_CHAR_RE.test(text[lead])) lead++;
      let trail = text.length;
      if (k < j) while (trail > lead && !LINK_CHAR_RE.test(text[trail - 1])) trail--;
      const cleaned = text.slice(lead, trail);
      // Empty-after-strip guard: a row reduced to no URL chars is pure
      // decoration sitting between two URLs — keep it verbatim so its borders
      // break the regex instead of merging distinct URLs into one broken href.
      const useLead = LINK_CHAR_RE.test(cleaned) ? lead : 0;
      const useText = useLead === lead ? cleaned : text;
      leadSkip.push(useLead);
      rowLens.push(useText.length);
      bases.push(joined.length);
      joined += useText;
    }
    // Painted links, lifted from row-local into joined-line offsets. Two halves
    // of one soft-wrapped link are adjacent here and share the core's run key,
    // so they fuse into ONE match: an overlapping regex match then decides
    // against the whole link instead of against one visual half.
    const painted: PaintedLink[] = [];
    for (let k = i; k <= j; k++) {
      const links = rows[k].links;
      if (links === undefined) continue;
      const g = k - i;
      const base = bases[g], off = leadSkip[g], len = rowLens[g];
      for (const p of links) {
        const s = Math.max(0, p.start - off);
        const e = Math.min(len, p.end - off);
        if (s >= e) continue;
        const last = painted[painted.length - 1];
        if (last !== undefined && last.key === p.key && last.end === base + s) last.end = base + e;
        else painted.push({ start: base + s, end: base + e, uri: p.uri, key: p.key });
      }
    }
    // Prefilter AFTER grouping so soft-wrap joining is unaffected: a logical
    // line with no hint character cannot match any pattern, and skipping it
    // takes the whole regex battery off the ordinary output row. Skipping also
    // leaves painted anchors exactly as they are, which is correct — with no
    // possible regex match there is nothing that could supersede one.
    if (joined.length > 0 && ROW_LINK_HINT.test(joined)) {
      for (const m of _findMatches(joined, painted, resolveFile, githubOwnerRepo)) {
        for (let k = i; k <= j; k++) {
          const base = bases[k - i];
          const rowEnd = base + rowLens[k - i];
          const s = Math.max(m.start, base);
          const e = Math.min(m.end, rowEnd);
          if (s < e) {
            const off = leadSkip[k - i];
            out.push(m.kind === "file"
              ? {
                  row: k, start: s - base + off, end: e - base + off,
                  url: m.url, kind: "file", hint: m.hint, source: m.source,
                }
              : { row: k, start: s - base + off, end: e - base + off, url: m.url });
          }
        }
      }
    }
    i = j + 1;
  }
  return out;
}

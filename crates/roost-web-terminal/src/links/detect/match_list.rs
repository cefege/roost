//! The match list of one joined logical line: painted producer links seed
//! precedence, then explicit schemes, GitHub refs and resolvable file paths
//! fill the gaps, blocked schemes reserving their text. `links::detect` splits
//! the surviving matches back into per-row segments.
//! Ports the match loop of `computeRowLinks` in
//! `apps/web/src/renderer/terminal-links.detect.ts`.

use super::{Match, MatchKind, PaintedLink, file_path, patterns};
use crate::link_target::{
    ResolveFile, TerminalLinkTarget, classify_terminal_link_target, is_explicit_file_name,
    is_windows_drive_absolute,
};

fn overlaps(matches: &[Match], start: usize, end: usize) -> bool {
    matches
        .iter()
        .any(|found| !(found.end <= start || found.start >= end))
}

fn has_exact(matches: &[Match], start: usize, end: usize) -> bool {
    matches
        .iter()
        .any(|found| found.start == start && found.end == end)
}

/// Drop every inferred match strictly inside `start..end`; painted ones stay.
fn evict_strict_substrings(matches: &mut Vec<Match>, start: usize, end: usize) {
    matches.retain(|found| {
        found.painted
            || found.start < start
            || found.end > end
            || (found.start == start && found.end == end)
    });
}

fn url_match(start: usize, end: usize, url: String, hint: String) -> Match {
    Match {
        start,
        end,
        url,
        kind: MatchKind::Url,
        hint: Some(hint),
        source: None,
        painted: false,
    }
}

fn ascii_text(text: &[u16]) -> String {
    String::from_utf16_lossy(text)
}

/// The link match list for one joined logical line, offsets absolute in it:
/// painted producer links first (an explicit producer URI beats inference),
/// then explicit schemes, GitHub refs and resolvable file paths filling gaps.
pub(super) fn find_matches(
    text: &[u16],
    painted: Vec<PaintedLink>,
    resolve_file: Option<ResolveFile<'_>>,
    github_owner_repo: Option<&str>,
) -> Vec<Match> {
    let mut matches: Vec<Match> = painted
        .into_iter()
        .map(|link| Match {
            painted: true,
            ..url_match(link.start, link.end, link.uri, String::new())
        })
        .collect();
    if text.contains(&u16::from(b':')) {
        seed_scheme_matches(text, &mut matches, resolve_file);
    }
    seed_github_matches(text, &mut matches, github_owner_repo);
    if let Some(resolve) = resolve_file {
        let mut from = 0;
        while let Some((start, end)) = file_path::next_file_path(text, from) {
            from = end;
            if overlaps(&matches, start, end) {
                continue;
            }
            let raw = ascii_text(&text[start..end]);
            if let Some(TerminalLinkTarget::WorkerFile {
                href: Some(href), ..
            }) = classify_terminal_link_target(&raw, Some(resolve))
            {
                let hint = format!("Open {raw}");
                matches.push(Match {
                    kind: MatchKind::File,
                    source: Some(raw),
                    ..url_match(start, end, href, hint)
                });
            }
        }
    }
    matches.sort_by_key(|found| found.start);
    // Blocked schemes only reserve their text against the file detector.
    matches.retain(|found| !found.painted && found.kind != MatchKind::Blocked);
    matches
}

/// Explicit schemes, scheme tokens without `//`, and `localhost:PORT` URLs.
/// Invalid or custom schemes stay as blocked ranges so a path-looking suffix
/// cannot be reinterpreted as an authenticated worker file.
fn seed_scheme_matches(
    text: &[u16],
    matches: &mut Vec<Match>,
    resolve_file: Option<ResolveFile<'_>>,
) {
    let mut from = 0;
    while let Some((start, end)) = patterns::next_scheme_url(text, from) {
        from = end;
        if has_exact(matches, start, end) {
            continue;
        }
        evict_strict_substrings(matches, start, end);
        if overlaps(matches, start, end) {
            continue;
        }
        let raw = ascii_text(&text[start..end]);
        let found = match classify_terminal_link_target(&raw, resolve_file) {
            Some(TerminalLinkTarget::External { href, display }) => {
                url_match(start, end, href, display)
            }
            Some(TerminalLinkTarget::WorkerFile {
                href: Some(href),
                display,
                ..
            }) => {
                let hint = format!("Open {display}");
                Match {
                    kind: MatchKind::File,
                    source: Some(raw),
                    ..url_match(start, end, href, hint)
                }
            }
            _ => Match {
                kind: MatchKind::Blocked,
                hint: None,
                ..url_match(start, end, raw, String::new())
            },
        };
        matches.push(found);
    }
    // `vscode:file/a.ts` is no external link, but its path-looking suffix must
    // not become a worker file; the boundary keeps `ts:9` of `foo.ts:9` out.
    from = 0;
    while let Some((start, end)) = patterns::next_scheme_token(text, from) {
        from = end;
        let raw = ascii_text(&text[start..end]);
        if is_explicit_file_name(&raw)
            || is_windows_drive_absolute(&raw)
            || patterns::is_loopback_origin(&text[start..end])
            || has_exact(matches, start, end)
            || overlaps(matches, start, end)
        {
            continue;
        }
        matches.push(Match {
            kind: MatchKind::Blocked,
            hint: None,
            ..url_match(start, end, raw, String::new())
        });
    }
    from = 0;
    while let Some((start, end)) = patterns::next_loopback_url(text, from) {
        from = end;
        if has_exact(matches, start, end) {
            continue;
        }
        evict_strict_substrings(matches, start, end);
        if overlaps(matches, start, end) {
            continue;
        }
        let raw = format!("http://{}", ascii_text(&text[start..end]));
        if let Some(TerminalLinkTarget::External { href, display }) =
            classify_terminal_link_target(&raw, None)
        {
            matches.push(url_match(start, end, href, display));
        }
    }
}

/// GitHub refs: `owner/repo#N` and `owner/repo@sha` always resolve; bare `#N`
/// and a bare SHA (7-40 hex with a letter) need the session's origin repo.
fn seed_github_matches(text: &[u16], matches: &mut Vec<Match>, github_owner_repo: Option<&str>) {
    let push = |matches: &mut Vec<Match>, start: usize, end: usize, url: String| {
        if !overlaps(matches, start, end) {
            matches.push(url_match(start, end, url.clone(), url));
        }
    };
    for (separator, path) in [(b'#', "issues"), (b'@', "commit")] {
        let mut from = 0;
        while let Some(found) = patterns::next_repo_ref(text, from, separator) {
            from = found.end;
            let group = |(start, end): (usize, usize)| ascii_text(&text[start..end]);
            let url = format!(
                "https://github.com/{}/{}/{path}/{}",
                group(found.owner),
                group(found.repo),
                group(found.reference)
            );
            push(matches, found.start, found.end, url);
        }
    }
    let Some(owner_repo) = github_owner_repo.filter(|owner_repo| !owner_repo.is_empty()) else {
        return;
    };
    let mut from = 0;
    while let Some(((start, end), digits)) = patterns::next_bare_issue(text, from) {
        from = end;
        let number = ascii_text(&text[digits.0..digits.1]);
        push(
            matches,
            start,
            end,
            format!("https://github.com/{owner_repo}/issues/{number}"),
        );
    }
    from = 0;
    while let Some((start, end)) = patterns::next_bare_sha(text, from) {
        from = end;
        let sha = ascii_text(&text[start..end]);
        push(
            matches,
            start,
            end,
            format!("https://github.com/{owner_repo}/commit/{sha}"),
        );
    }
}

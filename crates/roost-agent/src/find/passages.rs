//! Ported from oh-my-pi packages/coding-agent/src/tools/jfind/cascade.ts (MIT).
//! Owns hashline parsing, passage windowing, and compact passage sketches.
//! The parent find module uses these helpers during semantic candidate ranking.

const WINDOW_BYTES: usize = 8192;
const WINDOWS: usize = 24;

#[derive(Clone)]
pub(super) struct Passage {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) text: String,
    pub(super) lexical: f64,
}

pub(super) fn parse_hashline(content: &str) -> Vec<(usize, String)> {
    content
        .lines()
        .filter_map(|line| {
            let (number, text) = line.split_once(':')?;
            let number = number.parse::<usize>().ok()?;
            Some((number, text.strip_prefix(' ').unwrap_or(text).to_owned()))
        })
        .collect()
}

pub(super) fn make_windows(lines: &[(usize, String)], query: &str) -> Vec<Passage> {
    let mut windows = Vec::new();
    let mut start = 0;
    while start < lines.len() && windows.len() < WINDOWS {
        let mut end = start;
        let mut bytes = 0;
        while end < lines.len() {
            let next = lines[end].1.len() + 16;
            if end > start && bytes + next > WINDOW_BYTES {
                break;
            }
            bytes += next;
            end += 1;
        }
        let body = lines[start..end]
            .iter()
            .map(|(number, text)| format!("{number}: {text}"))
            .collect::<Vec<_>>()
            .join("\n");
        let query_words = query
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| word.len() > 2);
        let lower = body.to_lowercase();
        let lexical = query_words
            .map(|word| lower.matches(&word.to_lowercase()).count() as f64)
            .sum();
        windows.push(Passage {
            start: lines[start].0,
            end: lines[end - 1].0,
            text: body,
            lexical,
        });
        start = end;
    }
    windows.sort_by(|left_window, right_window| {
        right_window
            .lexical
            .total_cmp(&left_window.lexical)
            .then(left_window.start.cmp(&right_window.start))
    });
    windows.truncate(WINDOWS);
    windows
}

pub(super) fn sketch(passage: &Passage, query: &str, budget: usize) -> String {
    let needles: Vec<String> = query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.len() > 2)
        .map(str::to_lowercase)
        .collect();
    let mut lines: Vec<&str> = passage.text.lines().collect();
    lines.sort_by_key(|line| {
        std::cmp::Reverse(
            needles
                .iter()
                .filter(|needle| line.to_lowercase().contains(needle.as_str()))
                .count()
                + usize::from(line.contains('(')),
        )
    });
    let mut selected = Vec::new();
    let mut used = 0;
    for line in lines {
        let clipped: String = line.chars().take(180).collect();
        if used + clipped.len() > budget {
            continue;
        }
        used += clipped.len();
        selected.push(clipped);
    }
    selected.join("\n")
}

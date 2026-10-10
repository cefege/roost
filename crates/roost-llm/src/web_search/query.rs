//! Ported from oh-my-pi packages/coding-agent/src/web/search/query.ts (MIT).
//! Parses search operators once so engines can render supported syntax natively.
//! Post-filtering drops constraints only when their combined filter empties results.

use super::types::SearchSource;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
    pub excluded_terms: Vec<String>,
    pub sites: Vec<String>,
    pub excluded_sites: Vec<String>,
    pub in_urls: Vec<String>,
    pub excluded_in_urls: Vec<String>,
    pub in_titles: Vec<String>,
    pub excluded_in_titles: Vec<String>,
    pub in_text: Vec<String>,
    pub filetypes: Vec<String>,
    pub excluded_filetypes: Vec<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub original: String,
}

pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut parsed = ParsedQuery {
        original: raw.to_owned(),
        ..ParsedQuery::default()
    };
    let mut quoted = false;
    let mut token = String::new();
    let mut tokens = Vec::new();
    for character in raw.chars() {
        if matches!(character, '"' | '“' | '”') {
            quoted = !quoted;
            token.push(character);
        } else if character.is_whitespace() && !quoted {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    for token in tokens {
        let negative = token.starts_with('-');
        let directive_token = token.strip_prefix('-').unwrap_or(&token);
        let Some((operator, value)) = directive_token.split_once(':') else {
            let value = token.trim_matches(['"', '“', '”']);
            if value.eq_ignore_ascii_case("OR") {
                parsed.terms.push("OR".to_owned());
            } else if negative && value.len() > 1 {
                parsed.excluded_terms.push(value[1..].to_lowercase());
            } else if token.starts_with('"') || token.starts_with('“') {
                parsed.phrases.push(value.to_owned());
            } else if !value.is_empty() {
                parsed.terms.push(value.to_owned());
            }
            continue;
        };
        let value = value.trim_matches(['"', '“', '”']);
        if value.is_empty() {
            parsed.terms.push(token);
            continue;
        }
        match (operator.to_ascii_lowercase().as_str(), negative) {
            ("site", false) => parsed.sites.push(normalize_site(value)),
            ("site", true) => parsed.excluded_sites.push(normalize_site(value)),
            ("inurl", false) => parsed.in_urls.push(value.to_lowercase()),
            ("inurl", true) => parsed.excluded_in_urls.push(value.to_lowercase()),
            ("intitle", false) => parsed.in_titles.push(value.to_lowercase()),
            ("intitle", true) => parsed.excluded_in_titles.push(value.to_lowercase()),
            ("intext", false) => parsed.in_text.push(value.to_lowercase()),
            ("filetype", false) => parsed
                .filetypes
                .push(value.trim_start_matches('.').to_lowercase()),
            ("filetype", true) => parsed
                .excluded_filetypes
                .push(value.trim_start_matches('.').to_lowercase()),
            ("after", false) => parsed.after = parse_date(value),
            ("before", false) => parsed.before = parse_date(value),
            _ => parsed.terms.push(token),
        }
    }
    parsed
}

fn normalize_site(site: &str) -> String {
    site.to_lowercase()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_start_matches("*.")
        .trim_end_matches('/')
        .trim_end_matches('.')
        .to_owned()
}

fn parse_date(value: &str) -> Option<String> {
    let mut parts = value.split('-');
    let year = parts.next()?.parse::<u16>().ok()?;
    let month = parts.next()?.parse::<u8>().ok()?;
    let day = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

type ConstraintPredicate<'a> = Box<dyn Fn(&SearchSource) -> bool + 'a>;
pub fn apply_constraints(
    sources: Vec<SearchSource>,
    query: &ParsedQuery,
) -> (Vec<SearchSource>, Vec<String>) {
    let dimensions: Vec<(String, ConstraintPredicate<'_>)> = vec![
        (
            format_site_label(query),
            Box::new(|source| {
                query.sites.is_empty()
                    || query
                        .sites
                        .iter()
                        .any(|site| matches_site(&source.url, site))
            }),
        ),
        (
            format_excluded_site_label(query),
            Box::new(|source| {
                !query
                    .excluded_sites
                    .iter()
                    .any(|site| matches_site(&source.url, site))
            }),
        ),
        (
            format_constraint_label("inurl", &query.in_urls),
            Box::new(|source| {
                query
                    .in_urls
                    .iter()
                    .all(|term| source.url.to_lowercase().contains(term))
            }),
        ),
        (
            format_constraint_label("-inurl", &query.excluded_in_urls),
            Box::new(|source| {
                !query
                    .excluded_in_urls
                    .iter()
                    .any(|term| source.url.to_lowercase().contains(term))
            }),
        ),
        (
            format_constraint_label("intitle", &query.in_titles),
            Box::new(|source| {
                query
                    .in_titles
                    .iter()
                    .all(|term| source.title.to_lowercase().contains(term))
            }),
        ),
        (
            format_constraint_label("-intitle", &query.excluded_in_titles),
            Box::new(|source| {
                !query
                    .excluded_in_titles
                    .iter()
                    .any(|term| source.title.to_lowercase().contains(term))
            }),
        ),
        (
            format_constraint_label("filetype", &query.filetypes),
            Box::new(|source| {
                query.filetypes.is_empty()
                    || query
                        .filetypes
                        .iter()
                        .any(|ext| has_filetype(&source.url, ext))
            }),
        ),
        (
            format_constraint_label("-filetype", &query.excluded_filetypes),
            Box::new(|source| {
                !query
                    .excluded_filetypes
                    .iter()
                    .any(|ext| has_filetype(&source.url, ext))
            }),
        ),
        (
            format_date_label(query),
            Box::new(|source| matches_date(source, query)),
        ),
    ];
    let mut kept = sources;
    let mut notes = Vec::new();
    if kept.is_empty() {
        return (kept, notes);
    }
    for (label, matches) in dimensions {
        if label.is_empty() {
            continue;
        }
        let filtered: Vec<_> = kept
            .iter()
            .filter(|source| matches(source))
            .cloned()
            .collect();
        if filtered.is_empty() {
            notes.push(format!("Relaxed {label} because it removed every result."));
        } else {
            kept = filtered;
        }
    }
    (kept, notes)
}

fn format_site_label(query: &ParsedQuery) -> String {
    if query.sites.is_empty() {
        String::new()
    } else {
        format!("site:{}", query.sites.join(" OR site:"))
    }
}
fn format_excluded_site_label(query: &ParsedQuery) -> String {
    if query.excluded_sites.is_empty() {
        String::new()
    } else {
        format!("-site:{}", query.excluded_sites.join(" -site:"))
    }
}
fn format_constraint_label(operator: &str, values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        values
            .iter()
            .map(|value| format!("{operator}:{value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
fn format_date_label(query: &ParsedQuery) -> String {
    [
        query.after.as_ref().map(|date| format!("after:{date}")),
        query.before.as_ref().map(|date| format!("before:{date}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}
fn matches_site(url: &str, site: &str) -> bool {
    let url = url.to_lowercase();
    let Some(host) = url.split('/').nth(2) else {
        return false;
    };
    let (site_host, site_path) = site.split_once('/').unwrap_or((site, ""));
    let matches_host = host == site_host
        || host
            .strip_suffix(site_host)
            .is_some_and(|prefix| prefix.ends_with('.'));
    matches_host
        && (site_path.is_empty()
            || url
                .split('/')
                .skip(3)
                .collect::<Vec<_>>()
                .join("/")
                .starts_with(site_path))
}
fn has_filetype(url: &str, extension: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_lowercase()
        .ends_with(&format!(".{extension}"))
}
fn matches_date(source: &SearchSource, query: &ParsedQuery) -> bool {
    let Some(published) = source.published.as_deref() else {
        return true;
    };
    let published_day = published.get(..10).unwrap_or(published);
    query
        .after
        .as_ref()
        .is_none_or(|date| published_day >= date.as_str())
        && query
            .before
            .as_ref()
            .is_none_or(|date| published_day <= date.as_str())
}

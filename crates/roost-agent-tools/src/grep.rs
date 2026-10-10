//! Gitignore-aware regular-expression search for worker tool calls.
//! Called by `ToolHost`; results use shared hashline prefixes and update edit visibility.
//! Search traversal and matching use the same ignore and grep libraries as ripgrep.

use std::{fs, path::Path};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, sinks::UTF8};
use ignore::WalkBuilder;
use roost_protocol::wire::agent_chat::{GrepArgs, TOOL_GREP};
use serde_json::json;

use crate::{
    hashline::{EditStore, format_header, line_prefix, record_seen_lines},
    outcome::ToolOutcome,
};

const MATCH_LIMIT: usize = 200;

pub async fn grep_tool(store: &mut EditStore, cwd: &Path, args: GrepArgs) -> ToolOutcome {
    let working_store = store.clone();
    let cwd = cwd.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let mut working_store = working_store;
        let outcome = grep_tool_sync(&mut working_store, &cwd, args);
        (working_store, outcome)
    })
    .await
    {
        Ok((updated_store, outcome)) => {
            *store = updated_store;
            outcome
        }
        Err(error) => ToolOutcome::failure(format!("grep task failed: {error}")),
    }
}

fn grep_tool_sync(store: &mut EditStore, cwd: &Path, args: GrepArgs) -> ToolOutcome {
    let root = args
        .path
        .as_deref()
        .map_or_else(|| cwd.to_path_buf(), |path| cwd.join(path));
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(args.case_insensitive.unwrap_or(false))
        .build(&args.pattern)
    {
        Ok(matcher) => matcher,
        Err(error) => return ToolOutcome::failure(format!("invalid regular expression: {error}")),
    };
    let glob = match args.glob.as_deref().map(globset::Glob::new).transpose() {
        Ok(glob) => glob.map(|glob| glob.compile_matcher()),
        Err(error) => return ToolOutcome::failure(format!("invalid glob: {error}")),
    };
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .build();
    let mut output = String::new();
    let mut count = 0;
    for entry in walker {
        let entry = match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => entry,
            Ok(_) => continue,
            Err(error) => {
                output.push_str(&format!("walk error: {error}\n"));
                continue;
            }
        };
        let path = entry.path();
        if glob
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(path.strip_prefix(&root).unwrap_or(path)))
        {
            continue;
        }
        let mut matched_lines = Vec::new();
        let mut searcher = Searcher::new();
        let result = searcher.search_path(
            &matcher,
            path,
            UTF8(|line_number, line| {
                matched_lines.push((
                    u32::try_from(line_number).unwrap_or(u32::MAX),
                    line.trim_end_matches('\r').to_owned(),
                ));
                Ok(true)
            }),
        );
        if let Err(error) = result {
            output.push_str(&format!("{}: {error}\n", path.display()));
            continue;
        }
        if matched_lines.is_empty() {
            continue;
        }
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(_) => continue,
        };
        let tag = store.record_snapshot(path, &source);
        let relative = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
        output.push_str(&format!("{}\n", format_header(&relative, &tag)));
        let remaining = MATCH_LIMIT.saturating_sub(count);
        let matches: Vec<_> = matched_lines.iter().take(remaining).collect();
        let matching_numbers: std::collections::BTreeSet<u32> =
            matches.iter().map(|(line, _)| *line).collect();
        let mut shown = std::collections::BTreeSet::new();
        let context = args.context.unwrap_or_default();
        for line in &matching_numbers {
            shown.extend(line.saturating_sub(context).max(1)..=line.saturating_add(context));
        }
        let mut lines: Vec<_> = source.split('\n').collect();
        if source.ends_with('\n') {
            lines.pop();
        }
        let mut seen = Vec::new();
        for line_number in shown {
            if let Some(text) = line_number
                .checked_sub(1)
                .and_then(|index| lines.get(index as usize))
            {
                seen.push(line_number);
                output.push_str(&line_prefix(line_number, text.trim_end_matches('\r')));
                output.push('\n');
            }
        }
        count += matches.len();
        record_seen_lines(store, path, &seen);
        if count >= MATCH_LIMIT {
            break;
        }
    }
    if count >= MATCH_LIMIT {
        output.push_str(&format!("\nReached the {MATCH_LIMIT}-match limit."));
    }
    if output.is_empty() {
        output.push_str("No matches found.");
    }
    ToolOutcome::success(output).with_details(json!({"tool": TOOL_GREP, "matches": count}))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn ignores_gitignored_paths_and_formats_hashline_matches() {
        let directory = tempdir().expect("tempdir");
        fs::create_dir(directory.path().join(".git")).expect("git directory");
        fs::create_dir(directory.path().join("ignored")).expect("ignored directory");
        fs::write(directory.path().join(".gitignore"), "ignored/\n").expect("gitignore");
        fs::write(directory.path().join("visible.txt"), "match me\n").expect("visible file");
        fs::write(directory.path().join("ignored/hidden.txt"), "match me\n").expect("ignored file");
        let mut store = EditStore::default();
        let result = grep_tool(
            &mut store,
            directory.path(),
            GrepArgs {
                pattern: "match".to_owned(),
                path: None,
                glob: None,
                case_insensitive: None,
                context: None,
            },
        )
        .await;
        assert!(result.content.contains("visible.txt#"));
        assert!(result.content.contains("1:match me"));
        assert!(!result.content.contains("hidden.txt"));
    }
}

//! Ported from oh-my-pi packages/coding-agent/src/tools/jfind/cascade.ts (MIT).
//! Routes candidate files through native filename, sketch and passage judgments.
//! Worker glob/read tools supply the source tree and hashline-formatted contents.

use std::sync::Arc;

use roost_llm::{Answer, ModelInfo, Question};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::runtime::AgentRuntime;
use crate::tool_round::{CallResult, CallScope};
use crate::traits::{ToolCall, ToolOutcome};

const CANDIDATES: usize = 128;
const NAME_BATCH: usize = 64;
const FILES: usize = 20;
const WINDOW_BYTES: usize = 8192;
const WINDOWS: usize = 24;
const SKETCH_BYTES: usize = 384;
const CUTOFF: f64 = 0.45;
const THRESHOLD: f64 = 0.2;

#[derive(Deserialize)]
struct FindArgs {
    query: String,
    path: Option<String>,
}

#[derive(Clone)]
struct Passage {
    start: usize,
    end: usize,
    text: String,
    lexical: f64,
}

struct FilePlan {
    path: String,
    passages: Vec<Passage>,
}

#[derive(Clone)]
struct Hit {
    path: String,
    start: usize,
    end: usize,
    score: f64,
    snippet: String,
}

pub(crate) async fn available(runtime: &AgentRuntime, _model: &ModelInfo) -> bool {
    crate::judging::has_native_judge(runtime).await
}

pub(crate) async fn run_find(
    scope: &CallScope<'_>,
    args: &str,
    out: mpsc::Sender<String>,
) -> CallResult {
    let parsed = match serde_json::from_str::<FindArgs>(args) {
        Ok(parsed) if !parsed.query.trim().is_empty() => parsed,
        Ok(_) => return CallResult::failure("find query must not be empty"),
        Err(error) => return CallResult::failure(format!("invalid find arguments: {error}")),
    };
    let timeout = scope.runtime.inner.config.find_timeout;
    let partial_hits = Arc::new(Mutex::new(Vec::new()));
    match tokio::time::timeout(timeout, cascade(scope, &parsed, &out, partial_hits.clone())).await {
        Ok(Ok(hits)) => CallResult::plain(ToolOutcome {
            is_error: false,
            content: format_hits(hits),
            details_json: "{}".into(),
        }),
        Ok(Err(error)) => CallResult::failure(error),
        Err(_) => {
            tracing::debug!(conversation_id = %scope.record.id, "semantic find timed out");
            CallResult::plain(ToolOutcome {
                is_error: false,
                content: format_hits(partial_hits.lock().await.clone()),
                details_json: "{}".into(),
            })
        }
    }
}

async fn cascade(
    scope: &CallScope<'_>,
    args: &FindArgs,
    out: &mpsc::Sender<String>,
    partial_hits: Arc<Mutex<Vec<Hit>>>,
) -> Result<Vec<Hit>, String> {
    let glob_args = json!({"pattern":"**/*", "path":args.path});
    let listed = worker(scope, "glob", glob_args, out).await?;
    if listed.is_error {
        return Err(listed.content);
    }
    let mut paths: Vec<String> = listed
        .content
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("No matching files")
                && !line.starts_with("Results capped")
        })
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    paths.truncate(CANDIDATES);
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut name_scores = Vec::with_capacity(paths.len());
    for batch in paths.chunks(NAME_BATCH) {
        let questions = batch.iter().enumerate().map(|(idx, path)| {
            let key = format!("e{idx:03}");
            let name = path.rsplit('/').next().unwrap_or(path);
            Question::boolean(&key,
                &format!("Is the file tagged {key} ({name:?}) likely to contain what this search is looking for: {:?}? Judge by its name, size, and place in the tree; apply criteria.file.", args.query),
                "A file at this path plausibly contains code, text, or data matching the search.",
                "The file is unrelated by name and location; generated executable implementation can still be relevant.")
        }).collect();
        let state = json!({"search":args.query,"project":scope.record.cwd,"files":batch,"task":"Semantic grep over a source tree: locate files whose content matches the search description."});
        match crate::judging::judge(scope.runtime, None, state, questions).await {
            Ok(answers) => name_scores.extend(
                batch
                    .iter()
                    .enumerate()
                    .map(|(idx, _)| probability(&answers, &format!("e{idx:03}")).unwrap_or(0.0)),
            ),
            Err(error) => {
                tracing::debug!(%error, "semantic find filename judgment failed");
                name_scores.extend(std::iter::repeat_n(0.0, batch.len()));
            }
        }
    }

    let mut ranked: Vec<(usize, f64)> = (0..paths.len())
        .map(|idx| (idx, name_scores[idx]))
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| paths[left.0].cmp(&paths[right.0]))
    });
    ranked.truncate(FILES);
    let mut plans = Vec::new();
    for (idx, _) in ranked {
        if scope.cancel.is_cancelled() {
            break;
        }
        let read_args = json!({"path":paths[idx],"limit":200});
        let read = match worker(scope, "read", read_args, out).await {
            Ok(read) if !read.is_error => read,
            Ok(_) => continue,
            Err(error) => {
                tracing::debug!(%error, "semantic find read failed");
                continue;
            }
        };
        let content = parse_hashline(&read.content);
        let passages = make_windows(&content, &args.query);
        if !passages.is_empty() {
            plans.push(FilePlan {
                path: paths[idx].clone(),
                passages,
            });
        }
    }

    let mut sketches = Vec::new();
    for (file_idx, plan) in plans.iter().enumerate() {
        for (passage_idx, passage) in plan.passages.iter().enumerate() {
            sketches.push((
                file_idx,
                passage_idx,
                sketch(passage, &args.query, SKETCH_BYTES),
            ));
        }
    }
    let mut candidates = Vec::new();
    for batch in sketches.chunks(48) {
        let questions = batch.iter().enumerate().map(|(idx, (_, _, _))| {
            let key = format!("p{idx:02}");
            Question::boolean(&key, &format!("Could passage {key} implement a requested step of search? Apply criteria."),
                "Likely substantive implementation, definition or explanation of any part of the requested behavior. A matching helper for one step counts. Excerpts omit most source: favor recall.",
                "Unrelated code; mere mentions, declarations, call sites, tests or configuration without implementation.")
        }).collect();
        let state = json!({"criteria":{"no":"Unrelated code; mere mentions without implementation.","yes":"Substantive implementation or explanation."},"search":args.query,"cards":batch.iter().map(|(file_idx,_,text)| json!({"file":plans[*file_idx].path,"sketch":text})).collect::<Vec<_>>()});
        match crate::judging::judge(scope.runtime, None, state, questions).await {
            Ok(answers) => {
                for (idx, (file_idx, passage_idx, _)) in batch.iter().enumerate() {
                    let score = probability(&answers, &format!("p{idx:02}")).unwrap_or(1.0);
                    if score >= CUTOFF {
                        candidates.push((*file_idx, *passage_idx));
                    }
                }
            }
            Err(error) => {
                tracing::debug!(%error, "semantic find sketch judgment failed; keeping candidates");
                candidates.extend(
                    batch
                        .iter()
                        .map(|(file_idx, passage_idx, _)| (*file_idx, *passage_idx)),
                );
            }
        }
    }

    let mut hits = Vec::new();
    for (file_idx, passage_idx) in candidates {
        if scope.cancel.is_cancelled() {
            break;
        }
        let plan = &plans[file_idx];
        let passage = &plan.passages[passage_idx];
        let key = "p00";
        let question = Question::boolean(
            key,
            &format!(
                "Does passages.{key} substantively implement, define, or explain part of {:?}? Apply criteria.",
                args.query
            ),
            "This passage contains an implementation, definition, or substantive explanation of an important part of the search. A helper implementing one requested step counts.",
            "This passage only mentions, calls, imports, tests, or configures the subject, or contains unrelated code sharing keywords.",
        );
        let state = json!({"criteria":{"no":"Only mentions, calls, tests, imports, or config.","yes":"Substantive implementation, definition, or explanation."},"file":plan.path,"search":args.query,"passages":{"p00":passage.text}});
        let answer = crate::judging::judge(scope.runtime, None, state, vec![question]).await;
        let score = match answer {
            Ok(answers) => probability(&answers, key).unwrap_or(0.0),
            Err(error) => {
                tracing::debug!(%error, "semantic find passage judgment failed");
                0.0
            }
        };
        if score >= THRESHOLD {
            let snippet = passage
                .text
                .lines()
                .map(|line| line.split_once(':').map_or(line, |(_, text)| text.trim()))
                .find(|line| !line.is_empty())
                .unwrap_or("")
                .to_owned();
            hits.push(Hit {
                path: plan.path.clone(),
                start: passage.start,
                end: passage.end,
                score,
                snippet,
            });
            *partial_hits.lock().await = hits.clone();
        }
    }
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then(left.start.cmp(&right.start))
    });
    let _ = out
        .send(format!("Found {} semantic matches.\n", hits.len()))
        .await;
    Ok(hits)
}

async fn worker(
    scope: &CallScope<'_>,
    tool: &str,
    args: Value,
    out: &mpsc::Sender<String>,
) -> Result<ToolOutcome, String> {
    scope
        .runtime
        .inner
        .tools
        .execute(
            &scope.record.worker_fp,
            ToolCall {
                call_id: format!("find-{tool}"),
                conversation_id: scope.record.id.clone(),
                cwd: scope.record.cwd.clone(),
                tool: tool.to_owned(),
                args_json: args.to_string(),
                timeout_ms: 120_000,
            },
            out.clone(),
            scope.cancel.child_token(),
        )
        .await
}

fn probability(answers: &std::collections::BTreeMap<String, Answer>, key: &str) -> Option<f64> {
    match answers.get(key)? {
        Answer::Bool { probability } if probability.is_finite() => Some(*probability),
        _ => None,
    }
}

fn parse_hashline(content: &str) -> Vec<(usize, String)> {
    content
        .lines()
        .filter_map(|line| {
            let (number, text) = line.split_once(':')?;
            let number = number.parse::<usize>().ok()?;
            Some((number, text.strip_prefix(' ').unwrap_or(text).to_owned()))
        })
        .collect()
}

fn make_windows(lines: &[(usize, String)], query: &str) -> Vec<Passage> {
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
    windows.sort_by(|a, b| b.lexical.total_cmp(&a.lexical).then(a.start.cmp(&b.start)));
    windows.truncate(WINDOWS);
    windows
}

fn sketch(passage: &Passage, query: &str, budget: usize) -> String {
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

fn format_hits(mut hits: Vec<Hit>) -> String {
    if hits.is_empty() {
        return "No matching passages found.".into();
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.into_iter()
        .map(|hit| {
            format!(
                "{}:{}-{} {:.2} {}",
                hit.path, hit.start, hit.end, hit.score, hit.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

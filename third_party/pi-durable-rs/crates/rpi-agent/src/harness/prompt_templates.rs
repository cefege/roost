//! Port of `packages/agent/src/harness/prompt-templates.ts` @ pi 0.82.1
//! (2efa728) — the harness prompt-template loader (`loadPromptTemplates` /
//! `loadSourcedPromptTemplates`), `parseCommandArgs`, `substituteArgs`, and
//! `formatPromptTemplateInvocation`.
//!
//! This is the harness copy of the loader; the coding-agent copy
//! (`crate::rpi::core::prompt_templates` in the `rpi` crate) is a different
//! implementation — its `substituteArgs` additionally supports
//! `${N:-default}` defaults and full JS-`\s` tokenisation, which the harness
//! version does not (dependency direction: the harness layer must not call
//! the coding-agent layer).
//!
//! Intentional differences:
//! - Upstream `type: "warning"` on [`PromptTemplateDiagnostic`] is the only
//!   severity and is dropped (a Rust enum with one variant adds nothing;
//!   callers match on `code`).
//! - The frontmatter `argument-hint` field is parsed as part of the YAML
//!   value but ignored — upstream declares it in the interface yet never
//!   reads it, and the harness [`PromptTemplate`] type has no such field.
//! - A description that ends up empty maps to `None` (the harness type's
//!   `description` is optional; upstream assigns `""`).
//! - JS `length`/`slice` count UTF-16 code units; the description fallback
//!   truncation here counts Unicode scalar values (`char`s). Identical for
//!   BMP text.
//! - `parseCommandArgs` splits on space and tab only, exactly like upstream
//!   (the coding-agent copy's full JS-`\s` set is *not* used).
//! - JS `String.replace` with a *string* replacement expands `$&` / `$'` /
//!   `` $` `` sequences found in argument values; the Rust port inserts
//!   argument text literally (only matters when an argument itself contains
//!   those sequences).
//! - `load_prompt_templates` takes `&[String]` where upstream accepts
//!   `string | string[]` (no Rust equivalent of the union).
//! - `load_sourced_prompt_templates` requires the mapping closure instead of
//!   an optional `mapPromptTemplate`; identity mapping is
//!   `|template, _source| template`.

use crate::harness::types::{ExecutionEnv, FileErrorCode, FileInfo, FileKind, PromptTemplate};

// ---------------------------------------------------------------------------
// Diagnostics (prompt-templates.ts:4-16)
// ---------------------------------------------------------------------------

/// `PromptTemplateDiagnosticCode` (prompt-templates.ts:4) — stable diagnostic
/// codes emitted while loading prompt templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTemplateDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
}

impl PromptTemplateDiagnosticCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            PromptTemplateDiagnosticCode::FileInfoFailed => "file_info_failed",
            PromptTemplateDiagnosticCode::ListFailed => "list_failed",
            PromptTemplateDiagnosticCode::ReadFailed => "read_failed",
            PromptTemplateDiagnosticCode::ParseFailed => "parse_failed",
        }
    }
}

impl std::fmt::Display for PromptTemplateDiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `PromptTemplateDiagnostic` (prompt-templates.ts:7-16) — warning produced
/// while loading prompt templates. Upstream `type: "warning"` is the only
/// severity and is dropped (see module header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateDiagnostic {
    /// Stable diagnostic code.
    pub code: PromptTemplateDiagnosticCode,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Path associated with the diagnostic.
    pub path: String,
}

// ---------------------------------------------------------------------------
// Loading (prompt-templates.ts:24-165)
// ---------------------------------------------------------------------------

/// `loadPromptTemplates` (prompt-templates.ts:30-62) — load prompt templates
/// from one or more paths.
///
/// Directory inputs load direct `.md` children non-recursively. File inputs
/// load explicit `.md` files. Missing paths and non-markdown files are
/// skipped. Read and parse failures are returned as diagnostics.
pub async fn load_prompt_templates(
    env: &dyn ExecutionEnv,
    paths: &[String],
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        let info = match env.file_info(path, None).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    diagnostics.push(PromptTemplateDiagnostic {
                        code: PromptTemplateDiagnosticCode::FileInfoFailed,
                        message: error.message,
                        path: path.clone(),
                    });
                }
                continue;
            }
        };
        let kind = resolve_kind(env, &info, &mut diagnostics).await;
        if kind == Some(FileKind::Directory) {
            let (templates, template_diagnostics) = load_templates_from_dir(env, &info.path).await;
            prompt_templates.extend(templates);
            diagnostics.extend(template_diagnostics);
        } else if kind == Some(FileKind::File) && info.name.ends_with(".md") {
            // The `.md` check uses the addressed name (`info.name`), so a
            // symlink named `link.md` loads as a template (see test).
            let (template, template_diagnostics) = load_template_from_file(env, &info.path).await;
            if let Some(template) = template {
                prompt_templates.push(template);
            }
            diagnostics.extend(template_diagnostics);
        }
    }
    (prompt_templates, diagnostics)
}

/// One `{ path, source }` input of [`load_sourced_prompt_templates`]
/// (prompt-templates.ts:71-74).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateSourceInput<TSource> {
    pub path: String,
    pub source: TSource,
}

/// `{ promptTemplate, source }` entry of
/// [`load_sourced_prompt_templates`]'s result (prompt-templates.ts:74-77).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplate<TPromptTemplate, TSource> {
    pub prompt_template: TPromptTemplate,
    pub source: TSource,
}

/// `PromptTemplateDiagnostic & { source }` (prompt-templates.ts:77-78).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplateDiagnostic<TSource> {
    pub diagnostic: PromptTemplateDiagnostic,
    pub source: TSource,
}

/// `loadSourcedPromptTemplates` (prompt-templates.ts:70-93) — load prompt
/// templates from source-tagged paths.
///
/// Source values are preserved exactly and attached to every loaded prompt
/// template and diagnostic. The agent package does not interpret source
/// values; applications define their own provenance shape. The optional
/// `mapPromptTemplate` becomes the required `map_prompt_template` closure
/// (identity: `|prompt_template, _| prompt_template`).
pub async fn load_sourced_prompt_templates<TSource, TPromptTemplate, FMap>(
    env: &dyn ExecutionEnv,
    inputs: &[PromptTemplateSourceInput<TSource>],
    map_prompt_template: FMap,
) -> (
    Vec<SourcedPromptTemplate<TPromptTemplate, TSource>>,
    Vec<SourcedPromptTemplateDiagnostic<TSource>>,
)
where
    TSource: Clone,
    FMap: Fn(PromptTemplate, TSource) -> TPromptTemplate,
{
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    for input in inputs {
        let (loaded_templates, loaded_diagnostics) =
            load_prompt_templates(env, std::slice::from_ref(&input.path)).await;
        for prompt_template in loaded_templates {
            prompt_templates.push(SourcedPromptTemplate {
                prompt_template: map_prompt_template(prompt_template, input.source.clone()),
                source: input.source.clone(),
            });
        }
        for diagnostic in loaded_diagnostics {
            diagnostics.push(SourcedPromptTemplateDiagnostic {
                diagnostic,
                source: input.source.clone(),
            });
        }
    }
    (prompt_templates, diagnostics)
}

/// `loadTemplatesFromDir` (prompt-templates.ts:95-121): direct `.md`
/// children of `dir`, sorted by name, non-recursive. Unlike the skill
/// walker, dotfiles and `node_modules` are *not* skipped and no ignore
/// files apply.
async fn load_templates_from_dir(
    env: &dyn ExecutionEnv,
    dir: &str,
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut entries = match env.list_dir(dir, None).await {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(PromptTemplateDiagnostic {
                code: PromptTemplateDiagnosticCode::ListFailed,
                message: error.message,
                path: dir.to_string(),
            });
            return (prompt_templates, diagnostics);
        }
    };
    // Upstream sorts with `localeCompare`; plain byte order is the
    // deterministic equivalent.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in &entries {
        let kind = resolve_kind(env, entry, &mut diagnostics).await;
        if kind != Some(FileKind::File) || !entry.name.ends_with(".md") {
            continue;
        }
        let (template, template_diagnostics) = load_template_from_file(env, &entry.path).await;
        if let Some(template) = template {
            prompt_templates.push(template);
        }
        diagnostics.extend(template_diagnostics);
    }
    (prompt_templates, diagnostics)
}

/// `loadTemplateFromFile` (prompt-templates.ts:123-165). The name is the
/// basename minus a case-insensitive `.md` suffix; the description is the
/// frontmatter `description`, else the first non-empty body line truncated
/// to 60 chars with `...` appended when the line is longer.
async fn load_template_from_file(
    env: &dyn ExecutionEnv,
    file_path: &str,
) -> (Option<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut diagnostics = Vec::new();
    let raw_content = match env.read_text_file(file_path, None).await {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(PromptTemplateDiagnostic {
                code: PromptTemplateDiagnosticCode::ReadFailed,
                message: error.message,
                path: file_path.to_string(),
            });
            return (None, diagnostics);
        }
    };

    let (frontmatter, body) = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(PromptTemplateDiagnostic {
                code: PromptTemplateDiagnosticCode::ParseFailed,
                message: error.to_string(),
                path: file_path.to_string(),
            });
            return (None, diagnostics);
        }
    };

    // `body.split("\n").find(line => line.trim())` — first non-empty line
    // (prompt-templates.ts:151).
    let first_line = body.lines().find(|line| !line.trim().is_empty());
    let mut description = frontmatter.description;
    if description.is_empty()
        && let Some(first_line) = first_line
    {
        // JS `slice(0, 60)` counts UTF-16 units; chars are equivalent
        // for BMP text (see module header).
        description = first_line.chars().take(60).collect();
        if first_line.chars().count() > 60 {
            description.push_str("...");
        }
    }

    // `basenameEnvPath(filePath).replace(/\.md$/i, "")`
    // (prompt-templates.ts:159).
    let name = strip_md_suffix(&basename_env_path(file_path));
    let prompt_template = PromptTemplate {
        name,
        // Upstream assigns `""`; the optional harness type spells it `None`
        // (see module header).
        description: (!description.is_empty()).then_some(description),
        content: body,
    };
    (Some(prompt_template), diagnostics)
}

/// `PromptTemplateFrontmatter` (prompt-templates.ts:18-22) — only
/// `description` is read; `argument-hint` is declared upstream but never
/// used by the loader.
#[derive(Debug, Default)]
struct PromptTemplateFrontmatter {
    description: String,
}

/// `parseFrontmatter` (prompt-templates.ts:200-214): normalize newlines,
/// then split a leading `---` block; the body is trimmed only when a
/// frontmatter block was found. A YAML document that is not a mapping
/// behaves like upstream's `parse(yamlString) ?? {}`: the description reads
/// as absent. YAML syntax errors are reported as `parse_failed` diagnostics
/// by the caller.
fn parse_frontmatter(
    content: &str,
) -> Result<(PromptTemplateFrontmatter, String), serde_yaml::Error> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((PromptTemplateFrontmatter::default(), normalized));
    }
    // JS: normalized.indexOf("\n---", 3)
    let Some(end_index) = normalized[3..].find("\n---").map(|i| i + 3) else {
        return Ok((PromptTemplateFrontmatter::default(), normalized));
    };
    // JS: normalized.slice(4, endIndex) — empty when endIndex < 4.
    let yaml_string = normalized.get(4..end_index).unwrap_or("");
    // JS: normalized.slice(endIndex + 4).trim()
    let body = normalized[end_index + 4..].trim().to_string();

    let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_string)?;
    let mut frontmatter = PromptTemplateFrontmatter::default();
    if let serde_yaml::Value::Mapping(mapping) = parsed {
        // `typeof frontmatter.description === "string" ? ... : ""`
        // (prompt-templates.ts:152).
        frontmatter.description = mapping
            .get(serde_yaml::Value::String("description".to_string()))
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("")
            .to_string();
    }
    Ok((frontmatter, body))
}

/// `resolveKind` (prompt-templates.ts:167-198): follow a symlink to its
/// target kind. `not_found` failures are silent; other failures push a
/// `file_info_failed` diagnostic and resolve to `None`.
async fn resolve_kind(
    env: &dyn ExecutionEnv,
    info: &FileInfo,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
) -> Option<FileKind> {
    if info.kind == FileKind::File || info.kind == FileKind::Directory {
        return Some(info.kind);
    }
    let canonical_path = match env.canonical_path(&info.path, None).await {
        Ok(path) => path,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(PromptTemplateDiagnostic {
                    code: PromptTemplateDiagnosticCode::FileInfoFailed,
                    message: error.message,
                    path: info.path.clone(),
                });
            }
            return None;
        }
    };
    let target = match env.file_info(&canonical_path, None).await {
        Ok(target) => target,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(PromptTemplateDiagnostic {
                    code: PromptTemplateDiagnosticCode::FileInfoFailed,
                    message: error.message,
                    path: info.path.clone(),
                });
            }
            return None;
        }
    };
    if target.kind == FileKind::File || target.kind == FileKind::Directory {
        Some(target.kind)
    } else {
        None
    }
}

/// `basenameEnvPath` (prompt-templates.ts:216-220).
fn basename_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    match normalized.rfind('/') {
        Some(slash_index) => normalized[slash_index + 1..].to_string(),
        None => normalized.to_string(),
    }
}

/// `/\.md$/i` (prompt-templates.ts:159) — case-insensitive `.md` suffix
/// removal.
fn strip_md_suffix(name: &str) -> String {
    if name.len() >= 3 && name[name.len() - 3..].eq_ignore_ascii_case(".md") {
        name[..name.len() - 3].to_string()
    } else {
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// Argument tokenisation and substitution (prompt-templates.ts:222-267)
// ---------------------------------------------------------------------------

/// `parseCommandArgs` (prompt-templates.ts:223-246): parse an argument
/// string using simple shell-style single and double quotes. Only space and
/// tab separate tokens (upstream — not the JS `\s` set of the coding-agent
/// copy); quotes pair up without escapes, and an unterminated quote swallows
/// the rest of the input.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for char in args_string.chars() {
        match in_quote {
            Some(quote) => {
                if char == quote {
                    in_quote = None;
                } else {
                    current.push(char);
                }
            }
            None => {
                if char == '"' || char == '\'' {
                    in_quote = Some(char);
                } else if char == ' ' || char == '\t' {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(char);
                }
            }
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// `substituteArgs` (prompt-templates.ts:249-262): substitute prompt
/// template placeholders with command arguments.
///
/// Supported placeholders, in upstream replacement order (each pass runs
/// over the result of the previous one): `$N` (1-indexed positional,
/// missing → `""`), `${@:N}` / `${@:N:L}` (1-indexed slice, `0` clamped to
/// `1`, `L` optional length), `$ARGUMENTS` and `$@` (all args joined with
/// spaces). Argument values containing placeholders are not recursively
/// substituted beyond what the pass order implies (same as upstream).
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let result = replace_positional(content, args);
    let result = replace_slice_args(&result, args);
    let result = replace_literal(&result, "$ARGUMENTS", &all_args);
    replace_literal(&result, "$@", &all_args)
}

/// `/\$(\d+)/g` (prompt-templates.ts:251) — `$N` → `args[N-1]`, missing
/// (including `$0` and overflowing numbers) → `""`.
fn replace_positional(content: &str, args: &[String]) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    loop {
        let Some(pos) = rest.find('$') else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..pos]);
        let after_dollar = &rest[pos + 1..];
        let digits_len = after_dollar.bytes().take_while(u8::is_ascii_digit).count();
        if digits_len == 0 {
            result.push('$');
            rest = after_dollar;
            continue;
        }
        // JS `parseInt(num, 10) - 1` then `args[...] ?? ""`: 0 and
        // overflowing values index no argument.
        let index = after_dollar[..digits_len].parse::<usize>().ok();
        if let Some(argument) = index
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| args.get(i))
        {
            result.push_str(argument);
        }
        rest = &after_dollar[digits_len..];
    }
    result
}

/// `/\$\{@:(\d+)(?::(\d+))?\}/g` (prompt-templates.ts:252-257) —
/// `${@:N}` / `${@:N:L}` slices of the arguments. Candidates that do not
/// close with `}` stay literal and scanning resumes at the next `${@:`.
fn replace_slice_args(content: &str, args: &[String]) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    loop {
        let Some(pos) = rest.find("${@:") else {
            result.push_str(rest);
            break;
        };
        result.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        let start_digits: String = after
            .bytes()
            .take_while(u8::is_ascii_digit)
            .map(|byte| byte as char)
            .collect();
        if start_digits.is_empty() {
            result.push_str("${@:");
            rest = after;
            continue;
        }
        let mut tail = &after[start_digits.len()..];
        let mut length_digits: Option<String> = None;
        if let Some(after_colon) = tail.strip_prefix(':') {
            let len_digits: String = after_colon
                .bytes()
                .take_while(u8::is_ascii_digit)
                .map(|byte| byte as char)
                .collect();
            if len_digits.is_empty() {
                // `${@:N:...}` — the optional length group needs digits;
                // keep the literal text.
                result.push_str("${@:");
                result.push_str(&start_digits);
                result.push(':');
                rest = after_colon;
                continue;
            }
            let len_digits_len = len_digits.len();
            length_digits = Some(len_digits);
            tail = &after_colon[len_digits_len..];
        }
        match tail.strip_prefix('}') {
            Some(after_brace) => {
                // JS `parseInt(startStr, 10) - 1; if (start < 0) start = 0`
                // (prompt-templates.ts:253-254); overflowing numbers
                // saturate beyond any slice.
                let start = start_digits
                    .parse::<usize>()
                    .unwrap_or(usize::MAX)
                    .saturating_sub(1);
                let length = length_digits
                    .as_deref()
                    .and_then(|digits| digits.parse::<usize>().ok());
                let selected: Vec<&str> = args
                    .iter()
                    .skip(start)
                    .take(length.unwrap_or(usize::MAX))
                    .map(String::as_str)
                    .collect();
                result.push_str(&selected.join(" "));
                rest = after_brace;
            }
            None => {
                // Missing closing brace — not a placeholder; keep literal.
                result.push_str("${@:");
                result.push_str(&start_digits);
                if let Some(length_digits) = length_digits {
                    result.push(':');
                    result.push_str(&length_digits);
                }
                rest = tail;
            }
        }
    }
    result
}

/// Literal substring replacement (upstream `String.replace` with a string
/// replacement; see module header for the `$&`-expansion difference).
fn replace_literal(content: &str, needle: &str, replacement: &str) -> String {
    content.replace(needle, replacement)
}

/// `formatPromptTemplateInvocation` (prompt-templates.ts:265-267) — format a
/// prompt template invocation with positional arguments (upstream default
/// `args = []`; pass `&[]`).
pub fn format_prompt_template_invocation(template: &PromptTemplate, args: &[String]) -> String {
    substitute_args(&template.content, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::skills::test_env::MemoryEnv;

    fn arg(value: &str) -> String {
        value.to_string()
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn template(name: &str, description: Option<&str>, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            description: description.map(str::to_string),
            content: content.to_string(),
        }
    }

    /// Upstream `loads markdown templates non-recursively from one or more
    /// dirs` (prompt-templates.test.ts:13).
    #[tokio::test]
    async fn test_load_templates_non_recursive_from_dirs() {
        let env = MemoryEnv::new("/project");
        env.put_dir("a/nested");
        env.put_dir("b");
        env.put_file("a/one.md", "---\ndescription: One template\n---\nHello $1");
        env.put_file("a/nested/ignored.md", "Ignored");
        env.put_file("b/two.md", "First line description\nBody");

        let (prompt_templates, diagnostics) =
            load_prompt_templates(&env, &[arg("a"), arg("b")]).await;

        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            prompt_templates,
            vec![
                template("one", Some("One template"), "Hello $1"),
                template(
                    "two",
                    Some("First line description"),
                    "First line description\nBody"
                ),
            ]
        );
    }

    /// Upstream `preserves source info for sourced prompt templates`
    /// (prompt-templates.test.ts:31).
    #[tokio::test]
    async fn test_sourced_templates_preserve_source() {
        let env = MemoryEnv::new("/project");
        env.put_dir("prompts");
        env.put_file(
            "prompts/example.md",
            "---\ndescription: Example\n---\nExample body",
        );

        let (prompt_templates, diagnostics) = load_sourced_prompt_templates(
            &env,
            &[PromptTemplateSourceInput {
                path: arg("prompts"),
                source: "project".to_string(),
            }],
            |prompt_template, _source| prompt_template,
        )
        .await;

        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            prompt_templates,
            vec![SourcedPromptTemplate {
                prompt_template: template("example", Some("Example"), "Example body"),
                source: "project".to_string(),
            }]
        );
    }

    /// Upstream `attaches source info to diagnostics`
    /// (prompt-templates.test.ts:50) — a YAML syntax error fails the whole
    /// template load.
    #[tokio::test]
    async fn test_sourced_templates_attach_source_to_diagnostics() {
        let env = MemoryEnv::new("/project");
        env.put_file("broken.md", "---\ndescription: [unterminated\n---\nBody");

        let (prompt_templates, diagnostics) = load_sourced_prompt_templates(
            &env,
            &[PromptTemplateSourceInput {
                path: arg("broken.md"),
                source: "user".to_string(),
            }],
            |prompt_template, _source| prompt_template,
        )
        .await;

        // Upstream asserts the diagnostic's path and source
        // (prompt-templates.test.ts:61-65; `type: "warning"` is dropped, see
        // the module header); the code is `parse_failed` and the message is
        // the YAML parser's error text (`String(error.message)`,
        // prompt-templates.ts:161) — non-empty for a parse failure.
        assert_eq!(prompt_templates, Vec::new());
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.source, "user");
        assert_eq!(diagnostic.diagnostic.path, "/project/broken.md");
        assert_eq!(
            diagnostic.diagnostic.code,
            PromptTemplateDiagnosticCode::ParseFailed
        );
        assert!(
            !diagnostic.diagnostic.message.is_empty(),
            "parse_failed must carry the YAML error message"
        );
    }

    /// Upstream `loads explicit markdown files and symlinked files`
    /// (prompt-templates.test.ts:68). The symlink is loaded under its own
    /// name (`link`) with the target's content.
    #[tokio::test]
    async fn test_loads_explicit_files_and_symlinks() {
        let env = MemoryEnv::new("/project");
        env.put_file("target.md", "---\ndescription: Target\n---\nTarget body");
        env.put_symlink("/project/target.md", "/project/link.md");

        let (prompt_templates, _diagnostics) =
            load_prompt_templates(&env, &[arg("target.md"), arg("link.md")]).await;

        assert_eq!(
            prompt_templates,
            vec![
                template("target", Some("Target"), "Target body"),
                template("link", Some("Target"), "Target body"),
            ]
        );
    }

    /// `parseCommandArgs` (prompt-templates.ts:223-246): whitespace and
    /// quote handling.
    #[test]
    fn test_parse_command_args_quotes() {
        assert_eq!(parse_command_args(r#"a "b c" d"#), args(&["a", "b c", "d"]));
        assert_eq!(parse_command_args("'x y' z"), args(&["x y", "z"]));
        // Quotes preserved mid-token, removed from the result.
        assert_eq!(
            parse_command_args(r#"pre"mid dle"post"#),
            args(&["premid dlepost"])
        );
    }

    /// Quoted content is preserved without the quote characters; an
    /// unterminated quote swallows the rest; empty quotes produce no token;
    /// only space and tab split (upstream — not the JS `\s` set).
    #[test]
    fn test_parse_command_args_splitting() {
        assert_eq!(parse_command_args("a b  c"), args(&["a", "b", "c"]));
        assert_eq!(
            parse_command_args("  lead trail  "),
            args(&["lead", "trail"])
        );
        assert_eq!(parse_command_args(""), Vec::<String>::new());
        // Mixed quote kinds: the opening kind closes.
        assert_eq!(parse_command_args(r#""it's" ok"#), args(&["it's", "ok"]));
        // Unterminated quote swallows the rest.
        assert_eq!(parse_command_args(r#"a "bc"#), args(&["a", "bc"]));
        // Empty quoted string produces no token.
        assert_eq!(parse_command_args(r#"a "" b"#), args(&["a", "b"]));
        // Tabs split; U+FEFF does NOT (harness copy splits on space/tab only).
        assert_eq!(parse_command_args("a\tb"), args(&["a", "b"]));
        assert_eq!(parse_command_args("a\u{feff}b"), args(&["a\u{feff}b"]));
    }

    /// `$N`, `$@`, `$ARGUMENTS` (prompt-templates.ts:251, 258-260).
    #[test]
    fn test_substitute_args_positional_and_all() {
        let a = args(&["one", "two"]);
        assert_eq!(substitute_args("$1 $2", &a), "one two");
        // Missing positional → "" (upstream `?? ""`).
        assert_eq!(substitute_args("<$3>", &a), "<>");
        // `$0` indexes `args[-1]` → undefined → "".
        assert_eq!(substitute_args("<$0>", &a), "<>");
        assert_eq!(substitute_args("$@", &a), "one two");
        assert_eq!(substitute_args("$ARGUMENTS", &a), "one two");
        assert_eq!(substitute_args("$@", &[]), "");
    }

    /// `${@:N}` / `${@:N:L}` (prompt-templates.ts:252-257).
    #[test]
    fn test_substitute_args_slices() {
        let a = args(&["a", "b", "c", "d"]);
        assert_eq!(substitute_args("${@:2}", &a), "b c d");
        assert_eq!(substitute_args("${@:1}", &a), "a b c d");
        // `start < 0` clamps to 0 (0 treated as 1).
        assert_eq!(substitute_args("${@:0}", &a), "a b c d");
        assert_eq!(substitute_args("${@:2:2}", &a), "b c");
        // Length beyond the end clamps.
        assert_eq!(substitute_args("${@:3:99}", &a), "c d");
        assert_eq!(substitute_args("${@:1:0}", &a), "");
        assert_eq!(substitute_args("${@:99}", &a), "");
        // Malformed candidates stay literal.
        assert_eq!(substitute_args("${@:2:x}", &a), "${@:2:x}");
        assert_eq!(substitute_args("${@:2", &a), "${@:2");
        assert_eq!(substitute_args("${@:x}", &a), "${@:x}");
    }

    /// Substitution is single-pass per placeholder kind, in upstream order:
    /// values introduced by an earlier pass can be expanded by a later one.
    #[test]
    fn test_substitute_args_pass_order() {
        // `$1` expands first; the introduced `$@` is then expanded by the
        // final pass (upstream replace order, prompt-templates.ts:251-261).
        let a = args(&["$@", "x"]);
        assert_eq!(substitute_args("$1", &a), "$@ x");
        // A bare `$` and unknown `${...}` are not placeholders.
        let b = args(&["a"]);
        assert_eq!(
            substitute_args("cost: $5 and $ and ${foo}", &b),
            "cost:  and $ and ${foo}"
        );
    }

    /// Upstream `substitutes command arguments`
    /// (prompt-templates.test.ts:84) and the resource-formatting case
    /// (resource-formatting.test.ts:19).
    #[test]
    fn test_format_prompt_template_invocation_substitutes() {
        let one = template("one", None, "$1 ${@:2} $ARGUMENTS");
        assert_eq!(
            format_prompt_template_invocation(&one, &args(&["hello world", "test"])),
            "hello world test hello world test"
        );
        let review = template("review", None, "Review $1 with $ARGUMENTS");
        assert_eq!(
            format_prompt_template_invocation(&review, &args(&["a.ts", "care"])),
            "Review a.ts with a.ts care"
        );
    }
}

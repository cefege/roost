//! Port of `packages/agent/src/harness/skills.ts` @ pi 0.82.1 (2efa728) — the
//! harness skill loader (`loadSkills` / `loadSourcedSkills` /
//! `loadSkillsFromDirInternal`), diagnostics, and `formatSkillInvocation`.
//!
//! This is the harness copy of the loader; the coding-agent copy
//! (`crate::rpi::core::skills` in the `rpi` crate) is a different
//! implementation with a different signature and is intentionally not used
//! (dependency direction: the harness layer must not call the coding-agent
//! layer).
//!
//! Intentional differences:
//! - Upstream `type: "warning"` on [`SkillDiagnostic`] is the only severity
//!   and is dropped (a Rust enum with one variant adds nothing; callers match
//!   on `code`).
//! - The npm `ignore` matcher is replaced by the `ignore` crate's
//!   [`ignore::gitignore::Gitignore`]. Rule collection keeps upstream's
//!   prefix-rewriting (`prefixIgnorePattern`), so the anchored-pattern
//!   behavior is identical: a nested ignore file's rules apply only below its
//!   own directory, and later rules override earlier ones (last match wins,
//!   `!` whitelisting included). Pattern *syntax* follows gitignore
//!   (globset) instead of npm `ignore` where they disagree; malformed glob
//!   lines are skipped instead of being treated literally.
//! - Directory entries are matched with the `is_dir` flag instead of
//!   upstream's trailing-`/` path spelling (same gitignore semantics).
//! - Name/description length limits count Unicode scalar values; upstream
//!   counts UTF-16 code units (JS `string.length`). Identical for BMP text.
//! - Non-string YAML values for `name` / `description` /
//!   `disable-model-invocation` are treated as absent; upstream casts the
//!   whole document and reads properties, so behavior matches for everything
//!   except exotic values (JS would never see a non-boolean
//!   `disable-model-invocation` as true either).
//! - `load_skills` takes `&[String]` where upstream accepts
//!   `string | string[]` (no Rust equivalent of the union).
//! - `load_sourced_skills` requires the mapping closure instead of an
//!   optional `mapSkill`; identity mapping is `|skill, _source| skill`.
//! - `Skill.disable_model_invocation` is always `Some(boolean)` for loaded
//!   skills (upstream always assigns the field).

use async_trait::async_trait;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::harness::types::{ExecutionEnv, FileErrorCode, FileInfo, FileKind, Skill};

/// `MAX_NAME_LENGTH` (skills.ts:5).
const MAX_NAME_LENGTH: usize = 64;
/// `MAX_DESCRIPTION_LENGTH` (skills.ts:6).
const MAX_DESCRIPTION_LENGTH: usize = 1024;
/// `IGNORE_FILE_NAMES` (skills.ts:7) — ignore files honored at every level.
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

// ---------------------------------------------------------------------------
// Diagnostics (skills.ts:11-28)
// ---------------------------------------------------------------------------

/// `SkillDiagnosticCode` (skills.ts:11-16) — stable diagnostic codes emitted
/// while loading skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
    InvalidMetadata,
}

impl SkillDiagnosticCode {
    /// Upstream code literal.
    pub fn as_str(self) -> &'static str {
        match self {
            SkillDiagnosticCode::FileInfoFailed => "file_info_failed",
            SkillDiagnosticCode::ListFailed => "list_failed",
            SkillDiagnosticCode::ReadFailed => "read_failed",
            SkillDiagnosticCode::ParseFailed => "parse_failed",
            SkillDiagnosticCode::InvalidMetadata => "invalid_metadata",
        }
    }
}

impl std::fmt::Display for SkillDiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `SkillDiagnostic` (skills.ts:18-28) — warning produced while loading
/// skills. Upstream `type: "warning"` is the only severity and is dropped
/// (see module header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    /// Stable diagnostic code.
    pub code: SkillDiagnosticCode,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Path associated with the diagnostic.
    pub path: String,
}

// ---------------------------------------------------------------------------
// Formatting (skills.ts:38-41)
// ---------------------------------------------------------------------------

/// `formatSkillInvocation` (skills.ts:38-41) — format a skill invocation
/// prompt, optionally appending additional user instructions (appended only
/// when non-empty, matching upstream's truthiness check).
pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let skill_block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        dirname_env_path(&skill.file_path),
        skill.content
    );
    match additional_instructions {
        Some(instructions) if !instructions.is_empty() => {
            format!("{skill_block}\n\n{instructions}")
        }
        _ => skill_block,
    }
}

// ---------------------------------------------------------------------------
// Loading (skills.ts:43-175)
// ---------------------------------------------------------------------------

/// `loadSkills` (skills.ts:49-75) — load skills from one or more
/// directories.
///
/// Traverses directories recursively, loads `SKILL.md` files, loads direct
/// root `.md` files as skills, honors ignore files, and returns diagnostics
/// for invalid skill files. Missing input directories are skipped.
pub async fn load_skills(
    env: &dyn ExecutionEnv,
    dirs: &[String],
) -> (Vec<Skill>, Vec<SkillDiagnostic>) {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();
    for dir in dirs {
        let root_info = match env.file_info(dir, None).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    diagnostics.push(SkillDiagnostic {
                        code: SkillDiagnosticCode::FileInfoFailed,
                        message: error.message,
                        path: dir.clone(),
                    });
                }
                continue;
            }
        };
        if resolve_kind(env, &root_info, &mut diagnostics).await != Some(FileKind::Directory) {
            continue;
        }
        // Fresh ignore matcher per input dir (skills.ts:70).
        let mut ignore_matcher = IgnoreMatcher::new(&root_info.path);
        let mut walker = DirWalker {
            env,
            ignore_matcher: &mut ignore_matcher,
            root_dir: &root_info.path,
            skills: &mut skills,
            diagnostics: &mut diagnostics,
        };
        walker.load(&root_info.path, true).await;
    }
    (skills, diagnostics)
}

/// One `{ path, source }` input of [`load_sourced_skills`]
/// (skills.ts:84-87).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSourceInput<TSource> {
    pub path: String,
    pub source: TSource,
}

/// `{ skill, source }` entry of [`load_sourced_skills`]'s result
/// (skills.ts:87-90).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkill<TSkill, TSource> {
    pub skill: TSkill,
    pub source: TSource,
}

/// `SkillDiagnostic & { source }` (skills.ts:89-90).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkillDiagnostic<TSource> {
    pub diagnostic: SkillDiagnostic,
    pub source: TSource,
}

/// `loadSourcedSkills` (skills.ts:83-101) — load skills from source-tagged
/// directories.
///
/// Source values are preserved exactly and attached to every loaded skill
/// and diagnostic. The agent package does not interpret source values;
/// applications define their own provenance shape. The optional `mapSkill`
/// becomes the required `map_skill` closure (identity: `|skill, _| skill`).
pub async fn load_sourced_skills<TSource, TSkill, FMap>(
    env: &dyn ExecutionEnv,
    inputs: &[SkillSourceInput<TSource>],
    map_skill: FMap,
) -> (
    Vec<SourcedSkill<TSkill, TSource>>,
    Vec<SourcedSkillDiagnostic<TSource>>,
)
where
    TSource: Clone,
    FMap: Fn(Skill, TSource) -> TSkill,
{
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();
    for input in inputs {
        let (loaded_skills, loaded_diagnostics) =
            load_skills(env, std::slice::from_ref(&input.path)).await;
        for skill in loaded_skills {
            skills.push(SourcedSkill {
                skill: map_skill(skill, input.source.clone()),
                source: input.source.clone(),
            });
        }
        for diagnostic in loaded_diagnostics {
            diagnostics.push(SourcedSkillDiagnostic {
                diagnostic,
                source: input.source.clone(),
            });
        }
    }
    (skills, diagnostics)
}

/// Walker state shared across the recursive directory scan
/// (`loadSkillsFromDirInternal`, skills.ts:103-175). The trait method is
/// boxed, which also gives the recursive call its indirection.
struct DirWalker<'a> {
    env: &'a dyn ExecutionEnv,
    ignore_matcher: &'a mut IgnoreMatcher,
    root_dir: &'a str,
    skills: &'a mut Vec<Skill>,
    diagnostics: &'a mut Vec<SkillDiagnostic>,
}

#[async_trait]
trait DirLoader {
    /// One directory of the walk. A loadable `SKILL.md` makes the directory
    /// a skill root (first unsorted pass, skills.ts:137-149); otherwise the
    /// sorted second pass recurses into subdirectories and — only for the
    /// scan root — loads direct `.md` children as skills (skills.ts:151-172).
    async fn load(&mut self, dir: &str, include_root_files: bool);
}

#[async_trait]
impl DirLoader for DirWalker<'_> {
    /// One directory of the walk. A loadable `SKILL.md` makes the directory
    /// a skill root (first unsorted pass, skills.ts:137-149); otherwise the
    /// sorted second pass recurses into subdirectories and — only for the
    /// scan root — loads direct `.md` children as skills (skills.ts:151-172).
    async fn load(&mut self, dir: &str, include_root_files: bool) {
        let dir_info = match self.env.file_info(dir, None).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    self.diagnostics.push(SkillDiagnostic {
                        code: SkillDiagnosticCode::FileInfoFailed,
                        message: error.message,
                        path: dir.to_string(),
                    });
                }
                return;
            }
        };
        if resolve_kind(self.env, &dir_info, self.diagnostics).await != Some(FileKind::Directory) {
            return;
        }

        add_ignore_rules(
            self.env,
            self.ignore_matcher,
            dir,
            self.root_dir,
            self.diagnostics,
        )
        .await;

        let entries = match self.env.list_dir(dir, None).await {
            Ok(entries) => entries,
            Err(error) => {
                self.diagnostics.push(SkillDiagnostic {
                    code: SkillDiagnosticCode::ListFailed,
                    message: error.message,
                    path: dir.to_string(),
                });
                return;
            }
        };

        // First pass (skills.ts:137-149).
        for entry in &entries {
            if entry.name != "SKILL.md" {
                continue;
            }
            let full_path = entry.path.clone();
            if resolve_kind(self.env, entry, self.diagnostics).await != Some(FileKind::File) {
                continue;
            }
            let rel_path = relative_env_path(self.root_dir, &full_path);
            if self.ignore_matcher.ignores(&rel_path, false) {
                continue;
            }
            let (skill, skill_diagnostics) = load_skill_from_file(self.env, &full_path).await;
            if let Some(skill) = skill {
                self.skills.push(skill);
            }
            self.diagnostics.extend(skill_diagnostics);
            return;
        }

        // Second pass (skills.ts:151-172). Upstream sorts with
        // `localeCompare`; plain byte order is the deterministic equivalent.
        let mut sorted = entries;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in &sorted {
            if entry.name.starts_with('.') || entry.name == "node_modules" {
                continue;
            }
            let full_path = entry.path.clone();
            let Some(kind) = resolve_kind(self.env, entry, self.diagnostics).await else {
                continue;
            };
            let rel_path = relative_env_path(self.root_dir, &full_path);
            // Upstream spells directories as `${relPath}/`; the `is_dir`
            // flag carries the same information for the gitignore matcher.
            if self
                .ignore_matcher
                .ignores(&rel_path, kind == FileKind::Directory)
            {
                continue;
            }
            if kind == FileKind::Directory {
                self.load(&full_path, false).await;
                continue;
            }
            if kind != FileKind::File || !include_root_files || !entry.name.ends_with(".md") {
                continue;
            }
            let (skill, skill_diagnostics) = load_skill_from_file(self.env, &full_path).await;
            if let Some(skill) = skill {
                self.skills.push(skill);
            }
            self.diagnostics.extend(skill_diagnostics);
        }
    }
}

// ---------------------------------------------------------------------------
// Ignore handling (skills.ts:177-231)
// ---------------------------------------------------------------------------

/// `addIgnoreRules` (skills.ts:177-213): read `.gitignore` / `.ignore` /
/// `.fdignore` in `dir` and add their `prefixIgnorePattern`-rewritten lines
/// to the walk's shared matcher. Symlinked ignore files are skipped
/// (upstream: `info.value.kind !== "file"`), and non-`not_found` failures
/// become diagnostics.
async fn add_ignore_rules(
    env: &dyn ExecutionEnv,
    ignore_matcher: &mut IgnoreMatcher,
    dir: &str,
    root_dir: &str,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    let relative_dir = relative_env_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{relative_dir}/")
    };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = join_env_path(dir, filename);
        let info = match env.file_info(&ignore_path, None).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    diagnostics.push(SkillDiagnostic {
                        code: SkillDiagnosticCode::FileInfoFailed,
                        message: error.message,
                        path: ignore_path,
                    });
                }
                continue;
            }
        };
        if info.kind != FileKind::File {
            continue;
        }
        let content = match env.read_text_file(&ignore_path, None).await {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(SkillDiagnostic {
                    code: SkillDiagnosticCode::ReadFailed,
                    message: error.message,
                    path: ignore_path,
                });
                continue;
            }
        };
        // `/\r?\n/` split (skills.ts:207-210); a lone `\r` stays in the line.
        let patterns: Vec<String> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter_map(|line| prefix_ignore_pattern(line, &prefix))
            .collect();
        if !patterns.is_empty() {
            ignore_matcher.add_rules(patterns);
        }
    }
}

/// `prefixIgnorePattern` (skills.ts:215-231) — rewrite one ignore-file line
/// into a pattern relative to the walk root. Blank lines and comments
/// (except escaped `\#`) return `None`.
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;
    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest;
    }
    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest;
    }
    let prefixed = if prefix.is_empty() {
        pattern.to_string()
    } else {
        format!("{prefix}{pattern}")
    };
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

/// Accumulative root-relative ignore matcher (skills.ts:9 `IgnoreMatcher`).
///
/// All `prefixIgnorePattern`-rewritten rules from every visited directory
/// are collected in walk order into one matcher, so later rules override
/// earlier ones — the semantics of upstream's single npm `ignore` instance.
/// The `ignore` crate's [`Gitignore`] provides gitignore glob semantics
/// (anchored patterns after prefixing, `**/` for slash-less patterns, last
/// match wins, `!` whitelists).
struct IgnoreMatcher {
    root_dir: String,
    patterns: Vec<String>,
    built: Gitignore,
}

impl IgnoreMatcher {
    fn new(root_dir: &str) -> Self {
        IgnoreMatcher {
            root_dir: root_dir.to_string(),
            patterns: Vec::new(),
            built: Gitignore::empty(),
        }
    }

    fn add_rules(&mut self, patterns: Vec<String>) {
        self.patterns.extend(patterns);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let mut builder = GitignoreBuilder::new(&self.root_dir);
        for pattern in &self.patterns {
            // Malformed globs are skipped (npm `ignore` would treat them
            // leniently; only pathological lines differ).
            let _ = builder.add_line(None, pattern);
        }
        if let Ok(built) = builder.build() {
            self.built = built;
        }
    }

    /// `ignores(relPath)` (skills.ts:143, 158-159). `is_dir` replaces
    /// upstream's trailing-`/` path spelling for directories.
    fn ignores(&self, rel_path: &str, is_dir: bool) -> bool {
        self.built.matched(rel_path, is_dir).is_ignore()
    }
}

// ---------------------------------------------------------------------------
// Skill file loading (skills.ts:233-317)
// ---------------------------------------------------------------------------

/// `loadSkillFromFile` (skills.ts:233-279) — parse + validate one skill
/// file. A missing/empty description drops the skill (with a warning); every
/// other violation is a warning and the skill still loads.
async fn load_skill_from_file(
    env: &dyn ExecutionEnv,
    file_path: &str,
) -> (Option<Skill>, Vec<SkillDiagnostic>) {
    let mut diagnostics = Vec::new();
    let raw_content = match env.read_text_file(file_path, None).await {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(SkillDiagnostic {
                code: SkillDiagnosticCode::ReadFailed,
                message: error.message,
                path: file_path.to_string(),
            });
            return (None, diagnostics);
        }
    };

    let (frontmatter, body) = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(SkillDiagnostic {
                code: SkillDiagnosticCode::ParseFailed,
                message: error.to_string(),
                path: file_path.to_string(),
            });
            return (None, diagnostics);
        }
    };

    let skill_dir = dirname_env_path(file_path);
    let parent_dir_name = basename_env_path(&skill_dir);
    let description = frontmatter.description.clone();

    // Description errors are reported before name errors (skills.ts:255-263).
    for error in validate_description(description.as_deref()) {
        diagnostics.push(SkillDiagnostic {
            code: SkillDiagnosticCode::InvalidMetadata,
            message: error,
            path: file_path.to_string(),
        });
    }

    // `frontmatterName || parentDirName` (skills.ts:259-260): an empty
    // frontmatter name is falsy and falls back to the parent directory name.
    let name = match frontmatter.name.clone() {
        Some(name) if !name.is_empty() => name,
        _ => parent_dir_name.clone(),
    };
    for error in validate_name(&name, &parent_dir_name) {
        diagnostics.push(SkillDiagnostic {
            code: SkillDiagnosticCode::InvalidMetadata,
            message: error,
            path: file_path.to_string(),
        });
    }

    // Still load with warnings — unless the description is missing entirely
    // (skills.ts:265-267).
    let Some(description) = description.filter(|d| !d.trim().is_empty()) else {
        return (None, diagnostics);
    };

    (
        Some(Skill {
            name,
            description,
            content: body,
            file_path: file_path.to_string(),
            // `frontmatter["disable-model-invocation"] === true`
            // (skills.ts:275) — always assigned upstream.
            disable_model_invocation: Some(frontmatter.disable_model_invocation.unwrap_or(false)),
        }),
        diagnostics,
    )
}

/// `validateName` (skills.ts:281-291). Violations are warnings only — the
/// skill still loads.
fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    let len = name.chars().count();
    if len > MAX_NAME_LENGTH {
        errors.push(format!("name exceeds {MAX_NAME_LENGTH} characters ({len})"));
    }
    // /^[a-z0-9-]+$/ (skills.ts:285-287); the regex also fails on the empty
    // string.
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

/// `validateDescription` (skills.ts:293-301).
fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors = Vec::new();
    match description {
        None => errors.push("description is required".to_string()),
        Some(description) if description.trim().is_empty() => {
            errors.push("description is required".to_string());
        }
        Some(description) => {
            let len = description.chars().count();
            if len > MAX_DESCRIPTION_LENGTH {
                errors.push(format!(
                    "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({len})"
                ));
            }
        }
    }
    errors
}

/// `SkillFrontmatter` (skills.ts:30-35) — string fields read as absent for
/// non-string YAML values (upstream's unchecked record cast).
#[derive(Debug, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    disable_model_invocation: Option<bool>,
}

/// `parseFrontmatter` (skills.ts:303-317): normalize newlines, then split a
/// leading `---` block; the body is trimmed only when a frontmatter block
/// was found. A YAML document that is not a mapping behaves like upstream's
/// `parse(yamlString) ?? {}`: all fields read as absent. YAML syntax errors
/// are reported as `parse_failed` diagnostics by the caller.
fn parse_frontmatter(content: &str) -> Result<(SkillFrontmatter, String), serde_yaml::Error> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((SkillFrontmatter::default(), normalized));
    }
    // JS: normalized.indexOf("\n---", 3)
    let Some(end_index) = normalized[3..].find("\n---").map(|i| i + 3) else {
        return Ok((SkillFrontmatter::default(), normalized));
    };
    // JS: normalized.slice(4, endIndex) — empty when endIndex < 4.
    let yaml_string = normalized.get(4..end_index).unwrap_or("");
    // JS: normalized.slice(endIndex + 4).trim()
    let body = normalized[end_index + 4..].trim().to_string();

    let parsed: serde_yaml::Value = serde_yaml::from_str(yaml_string)?;
    let mut frontmatter = SkillFrontmatter::default();
    if let serde_yaml::Value::Mapping(mapping) = parsed {
        let get = |key: &str| mapping.get(serde_yaml::Value::String(key.to_string()));
        frontmatter.name = get("name")
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string);
        frontmatter.description = get("description")
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string);
        // Strict booleans only: upstream `=== true` ignores strings like
        // `"true"` and YAML 1.1-style `yes` (serde_yaml parses those as
        // strings).
        frontmatter.disable_model_invocation =
            get("disable-model-invocation").and_then(serde_yaml::Value::as_bool);
    }
    Ok((frontmatter, body))
}

// ---------------------------------------------------------------------------
// Kind resolution and path helpers (skills.ts:319-375)
// ---------------------------------------------------------------------------

/// `resolveKind` (skills.ts:319-350): follow a symlink to its target kind.
/// `not_found` failures are silent; other failures push a
/// `file_info_failed` diagnostic and resolve to `None`.
async fn resolve_kind(
    env: &dyn ExecutionEnv,
    info: &FileInfo,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<FileKind> {
    if info.kind == FileKind::File || info.kind == FileKind::Directory {
        return Some(info.kind);
    }
    let canonical_path = match env.canonical_path(&info.path, None).await {
        Ok(path) => path,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(SkillDiagnostic {
                    code: SkillDiagnosticCode::FileInfoFailed,
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
                diagnostics.push(SkillDiagnostic {
                    code: SkillDiagnosticCode::FileInfoFailed,
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

/// `joinEnvPath` (skills.ts:352-354).
fn join_env_path(base: &str, child: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        child.trim_start_matches('/')
    )
}

/// `dirnameEnvPath` (skills.ts:356-360) — `"/"` for a rootless path (upstream
/// `slashIndex <= 0`).
fn dirname_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    match normalized.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(slash_index) => normalized[..slash_index].to_string(),
    }
}

/// `basenameEnvPath` (skills.ts:362-366).
fn basename_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    match normalized.rfind('/') {
        Some(slash_index) => normalized[slash_index + 1..].to_string(),
        None => normalized.to_string(),
    }
}

/// `relativeEnvPath` (skills.ts:368-375): root-relative spelling, or the
/// path with leading slashes trimmed when it is outside the root.
fn relative_env_path(root: &str, path: &str) -> String {
    let normalized_root = root.trim_end_matches('/');
    let normalized_path = path.trim_end_matches('/');
    if normalized_path == normalized_root {
        return String::new();
    }
    if normalized_path.starts_with(&format!("{normalized_root}/")) {
        // The prefix match guarantees `normalized_root.len() + 1` is a char
        // boundary.
        normalized_path[normalized_root.len() + 1..].to_string()
    } else {
        normalized_path.trim_start_matches('/').to_string()
    }
}

#[cfg(test)]
pub(crate) mod test_env {
    //! In-memory [`ExecutionEnv`] for harness resource loader tests (used
    //! from the sibling `prompt_templates.rs` test module too).

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::harness::types::{
        CreateDirOptions, CreateTempFileOptions, ExecutionError, ExecutionErrorCode, FileError,
        FileErrorCode, FileInfo, FileKind, FileSystem, ReadTextLinesOptions, RemoveOptions, Shell,
        ShellExecOptions, ShellExecResult,
    };

    /// One filesystem object of [`MemoryEnv`]. Symlink targets are addressed
    /// paths (absolute, or relative to the link's parent).
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Entry {
        File(String),
        Dir,
        Symlink(String),
    }

    /// Minimal in-memory filesystem + shell for tests. `file_info` /
    /// `list_dir` report the addressed spelling (symlinks not followed);
    /// `read_text_file` / `list_dir` / `canonical_path` resolve symlink
    /// chains, including intermediate components. Paths are stored keyed by
    /// absolute addressed path.
    pub(crate) struct MemoryEnv {
        root: String,
        entries: Mutex<BTreeMap<String, Entry>>,
    }

    impl MemoryEnv {
        pub(crate) fn new(root: &str) -> Self {
            let mut entries = BTreeMap::new();
            entries.insert(root.to_string(), Entry::Dir);
            MemoryEnv {
                root: root.to_string(),
                entries: Mutex::new(entries),
            }
        }

        /// Absolute addressed spelling for `path` (relative inputs resolve
        /// against the root).
        fn abs(&self, path: &str) -> String {
            if path.starts_with('/') {
                path.trim_end_matches('/').to_string()
            } else {
                format!("{}/{}", self.root, path.trim_end_matches('/'))
            }
        }

        /// Resolve symlink chains component by component.
        fn resolve(&self, path: &str) -> String {
            let abs = self.abs(path);
            let entries = self.entries.lock().unwrap();
            let mut current = String::new();
            for component in abs.trim_start_matches('/').split('/') {
                if component.is_empty() {
                    continue;
                }
                current = format!("{current}/{component}");
                if let Some(Entry::Symlink(target)) = entries.get(&current) {
                    current = if target.starts_with('/') {
                        target.clone()
                    } else {
                        let parent = current
                            .rfind('/')
                            .map(|i| current[..i].to_string())
                            .unwrap_or_default();
                        format!("{parent}/{target}")
                    };
                }
            }
            current
        }

        fn ensure_dir(entries: &mut BTreeMap<String, Entry>, path: &str) {
            let mut parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
            let mut current = String::new();
            for part in parts.drain(..) {
                current = format!("{current}/{part}");
                entries.entry(current.clone()).or_insert(Entry::Dir);
            }
        }

        pub(crate) fn put_dir(&self, path: &str) {
            let abs = self.abs(path);
            let mut entries = self.entries.lock().unwrap();
            Self::ensure_dir(&mut entries, &abs);
        }

        pub(crate) fn put_file(&self, path: &str, content: &str) {
            let abs = self.abs(path);
            let mut entries = self.entries.lock().unwrap();
            if let Some(parent) = abs.rfind('/').map(|i| abs[..i].to_string()) {
                Self::ensure_dir(&mut entries, &parent);
            }
            entries.insert(abs, Entry::File(content.to_string()));
        }

        pub(crate) fn put_symlink(&self, target: &str, link: &str) {
            let abs = self.abs(link);
            let mut entries = self.entries.lock().unwrap();
            if let Some(parent) = abs.rfind('/').map(|i| abs[..i].to_string()) {
                Self::ensure_dir(&mut entries, &parent);
            }
            entries.insert(abs, Entry::Symlink(target.to_string()));
        }

        fn info_for(&self, name: &str, path: &str, entry: &Entry) -> FileInfo {
            let (kind, size) = match entry {
                Entry::File(content) => (FileKind::File, content.len() as u64),
                Entry::Dir => (FileKind::Directory, 0),
                Entry::Symlink(_) => (FileKind::Symlink, 0),
            };
            FileInfo {
                name: name.to_string(),
                path: path.to_string(),
                kind,
                size,
                mtime_ms: 0.0,
            }
        }
    }

    #[async_trait]
    impl FileSystem for MemoryEnv {
        fn cwd(&self) -> &str {
            &self.root
        }

        async fn absolute_path(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<String, FileError> {
            Ok(self.abs(path))
        }

        async fn join_path(
            &self,
            parts: &[String],
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<String, FileError> {
            Ok(parts.join("/"))
        }

        async fn read_text_file(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<String, FileError> {
            let resolved = self.resolve(path);
            let entries = self.entries.lock().unwrap();
            match entries.get(&resolved) {
                Some(Entry::File(content)) => Ok(content.clone()),
                _ => Err(FileError::new(
                    FileErrorCode::NotFound,
                    format!("not found: {path}"),
                )),
            }
        }

        async fn read_text_lines(
            &self,
            path: &str,
            _options: ReadTextLinesOptions,
        ) -> Result<Vec<String>, FileError> {
            let content = self.read_text_file(path, None).await?;
            Ok(content.split('\n').map(str::to_string).collect())
        }

        async fn read_binary_file(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<Vec<u8>, FileError> {
            let content = self.read_text_file(path, None).await?;
            Ok(content.into_bytes())
        }

        async fn write_file(
            &self,
            path: &str,
            content: &[u8],
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<(), FileError> {
            let content = String::from_utf8(content.to_vec()).map_err(|error| {
                FileError::new(FileErrorCode::Invalid, format!("non-utf8 content: {error}"))
            })?;
            self.put_file(path, &content);
            Ok(())
        }

        async fn append_file(
            &self,
            path: &str,
            content: &[u8],
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<(), FileError> {
            let append = String::from_utf8(content.to_vec()).map_err(|error| {
                FileError::new(FileErrorCode::Invalid, format!("non-utf8 content: {error}"))
            })?;
            let existing = self.read_text_file(path, None).await.unwrap_or_default();
            self.put_file(path, &format!("{existing}{append}"));
            Ok(())
        }

        async fn rename_file(
            &self,
            source_path: &str,
            destination_path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<(), FileError> {
            let content = self.read_text_file(source_path, None).await?;
            self.put_file(destination_path, &content);
            self.remove(
                source_path,
                RemoveOptions {
                    recursive: false,
                    force: false,
                    abort_signal: None,
                },
            )
            .await?;
            Ok(())
        }

        async fn file_info(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<FileInfo, FileError> {
            let abs = self.abs(path);
            // Resolve the parent chain, keep the final component unresolved.
            let (parent, name) = match abs.rfind('/') {
                Some(i) => (abs[..i].to_string(), abs[i + 1..].to_string()),
                None => (String::new(), abs.clone()),
            };
            let full = if parent.is_empty() {
                abs.clone()
            } else {
                format!("{}/{name}", self.resolve(&parent))
            };
            let entries = self.entries.lock().unwrap();
            let entry = entries.get(&full).ok_or_else(|| {
                FileError::new(FileErrorCode::NotFound, format!("not found: {path}"))
            })?;
            Ok(self.info_for(&name, &abs, entry))
        }

        async fn list_dir(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<Vec<FileInfo>, FileError> {
            let resolved = self.resolve(path);
            let entries = self.entries.lock().unwrap();
            if !matches!(entries.get(&resolved), Some(Entry::Dir)) {
                return Err(FileError::new(
                    FileErrorCode::NotFound,
                    format!("not a directory: {path}"),
                ));
            }
            let prefix = format!("{resolved}/");
            let mut infos = Vec::new();
            for (key, entry) in entries.iter() {
                if let Some(name) = key.strip_prefix(&prefix) {
                    if name.contains('/') {
                        continue;
                    }
                    // Children are reported under the addressed spelling of
                    // the queried directory (through symlinks).
                    let child_path = format!("{}/{name}", path.trim_end_matches('/'));
                    infos.push(self.info_for(name, &child_path, entry));
                }
            }
            Ok(infos)
        }

        async fn canonical_path(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<String, FileError> {
            let resolved = self.resolve(path);
            let entries = self.entries.lock().unwrap();
            if entries.contains_key(&resolved) {
                Ok(resolved)
            } else {
                Err(FileError::new(
                    FileErrorCode::NotFound,
                    format!("not found: {path}"),
                ))
            }
        }

        async fn exists(
            &self,
            path: &str,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<bool, FileError> {
            let resolved = self.resolve(path);
            Ok(self.entries.lock().unwrap().contains_key(&resolved))
        }

        async fn create_dir(
            &self,
            path: &str,
            _options: CreateDirOptions,
        ) -> Result<(), FileError> {
            self.put_dir(path);
            Ok(())
        }

        async fn remove(&self, path: &str, options: RemoveOptions) -> Result<(), FileError> {
            let abs = self.abs(path);
            let mut entries = self.entries.lock().unwrap();
            if options.recursive {
                let prefix = format!("{abs}/");
                entries.retain(|key, _| key != &abs && !key.starts_with(&prefix));
            } else {
                entries.remove(&abs);
            }
            Ok(())
        }

        async fn create_temp_dir(
            &self,
            _prefix: Option<&str>,
            _abort_signal: Option<tokio_util::sync::CancellationToken>,
        ) -> Result<String, FileError> {
            Err(FileError::new(
                FileErrorCode::NotSupported,
                "temp dirs not supported",
            ))
        }

        async fn create_temp_file(
            &self,
            _options: CreateTempFileOptions,
        ) -> Result<String, FileError> {
            Err(FileError::new(
                FileErrorCode::NotSupported,
                "temp files not supported",
            ))
        }

        async fn cleanup(&self) {}
    }

    #[async_trait]
    impl Shell for MemoryEnv {
        async fn exec(
            &self,
            _command: &str,
            _options: Option<ShellExecOptions>,
        ) -> Result<ShellExecResult, ExecutionError> {
            Err(ExecutionError::new(
                ExecutionErrorCode::Unknown,
                "shell not supported in tests",
            ))
        }

        async fn cleanup(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::MemoryEnv;
    use super::*;

    fn arg(path: &str) -> String {
        path.to_string()
    }

    fn skill(
        name: &str,
        description: &str,
        content: &str,
        file_path: &str,
        disable_model_invocation: Option<bool>,
    ) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            file_path: file_path.to_string(),
            disable_model_invocation,
        }
    }

    const EXAMPLE_SKILL: &str =
        "---\nname: example\ndescription: Example skill\n---\nUse this skill.";

    /// Upstream `loads SKILL.md files through the execution environment`
    /// (skills.test.ts:9).
    #[tokio::test]
    async fn test_load_skills_through_execution_env() {
        let env = MemoryEnv::new("/project");
        env.put_dir(".agents/skills/example");
        env.put_file(
            ".agents/skills/example/SKILL.md",
            "---\nname: example\ndescription: Example skill\ndisable-model-invocation: true\n---\nUse this skill.\n",
        );

        let (skills, diagnostics) = load_skills(&env, &[arg(".agents/skills")]).await;

        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            skills,
            vec![skill(
                "example",
                "Example skill",
                "Use this skill.",
                "/project/.agents/skills/example/SKILL.md",
                Some(true),
            )]
        );
    }

    /// Upstream `loads skills through symlinked directories`
    /// (skills.test.ts:38). The skill's `filePath` stays under the
    /// addressed (link) spelling.
    #[tokio::test]
    async fn test_load_skills_through_symlinked_directories() {
        let env = MemoryEnv::new("/project");
        env.put_dir("actual/example");
        env.put_file("actual/example/SKILL.md", EXAMPLE_SKILL);
        env.put_symlink("/project/actual", "/project/skills-link");

        let (skills, _diagnostics) = load_skills(&env, &[arg("skills-link")]).await;

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["example"]
        );
        assert_eq!(skills[0].file_path, "/project/skills-link/example/SKILL.md");
    }

    /// Upstream `preserves source info for sourced skills`
    /// (skills.test.ts:54).
    #[tokio::test]
    async fn test_sourced_skills_preserve_source() {
        let env = MemoryEnv::new("/project");
        env.put_dir("user/example");
        env.put_file("user/example/SKILL.md", EXAMPLE_SKILL);

        let (skills, diagnostics) = load_sourced_skills(
            &env,
            &[SkillSourceInput {
                path: arg("user"),
                source: "user".to_string(),
            }],
            |skill, _source| skill,
        )
        .await;

        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            skills,
            vec![SourcedSkill {
                skill: skill(
                    "example",
                    "Example skill",
                    "Use this skill.",
                    "/project/user/example/SKILL.md",
                    Some(false),
                ),
                source: "user".to_string(),
            }]
        );
    }

    /// Upstream `attaches source info to diagnostics` (skills.test.ts:82).
    #[tokio::test]
    async fn test_sourced_skills_attach_source_to_diagnostics() {
        let env = MemoryEnv::new("/project");
        env.put_dir("user/broken");
        env.put_file(
            "user/broken/SKILL.md",
            "---\nname: broken\n---\nMissing description.",
        );

        let (skills, diagnostics) = load_sourced_skills(
            &env,
            &[SkillSourceInput {
                path: arg("user"),
                source: "user".to_string(),
            }],
            |skill, _source| skill,
        )
        .await;

        assert_eq!(skills, Vec::new());
        assert_eq!(
            diagnostics,
            vec![SourcedSkillDiagnostic {
                diagnostic: SkillDiagnostic {
                    code: SkillDiagnosticCode::InvalidMetadata,
                    message: "description is required".to_string(),
                    path: "/project/user/broken/SKILL.md".to_string(),
                },
                source: "user".to_string(),
            }]
        );
    }

    /// Upstream `loads direct markdown children only from the root
    /// directory` (skills.test.ts:104). The skill name falls back to the
    /// parent directory name (`skills`).
    #[tokio::test]
    async fn test_load_skills_direct_root_markdown_only() {
        let env = MemoryEnv::new("/project");
        env.put_dir("skills/nested");
        env.put_file(
            "skills/root.md",
            "---\ndescription: Root skill\n---\nRoot content",
        );
        env.put_file(
            "skills/nested/ignored.md",
            "---\ndescription: Ignored\n---\nIgnored content",
        );

        let (skills, _diagnostics) = load_skills(&env, &[arg("skills")]).await;

        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["skills"]
        );
        assert_eq!(skills[0].content, "Root content");
    }

    /// `.gitignore` chains skip matching skill directories (skills.ts:177-213).
    #[tokio::test]
    async fn test_load_skills_honors_ignore_files() {
        let env = MemoryEnv::new("/project");
        env.put_dir("skills/hidden");
        env.put_file("skills/hidden/SKILL.md", EXAMPLE_SKILL);
        env.put_dir("skills/visible");
        env.put_file(
            "skills/visible/SKILL.md",
            "---\nname: visible\ndescription: Visible skill\n---\nVisible content",
        );
        env.put_file("skills/.gitignore", "hidden/\n");

        let (skills, diagnostics) = load_skills(&env, &[arg("skills")]).await;

        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            skills
                .iter()
                .map(|s| s.file_path.as_str())
                .collect::<Vec<_>>(),
            ["/project/skills/visible/SKILL.md"]
        );
    }

    /// Negation rules and later-rule-wins ordering, including root `.md`
    /// skills (skills.ts:215-231).
    #[tokio::test]
    async fn test_load_skills_ignore_negation_whitelists() {
        let env = MemoryEnv::new("/project");
        env.put_dir("skills");
        env.put_file(
            "skills/root.md",
            "---\ndescription: Root skill\n---\nRoot content",
        );
        env.put_file(
            "skills/keep.md",
            "---\ndescription: Keep skill\n---\nKeep content",
        );
        env.put_file("skills/.gitignore", "*.md\n!keep.md\n");

        let (skills, _diagnostics) = load_skills(&env, &[arg("skills")]).await;

        assert_eq!(
            skills
                .iter()
                .map(|s| s.file_path.as_str())
                .collect::<Vec<_>>(),
            ["/project/skills/keep.md"]
        );
    }

    /// Name validation is warning-only: the skill still loads, with an
    /// `invalid_metadata` diagnostic per violation (skills.ts:281-291).
    #[tokio::test]
    async fn test_load_skills_name_validation_warns() {
        let env = MemoryEnv::new("/project");
        env.put_dir("skills/a");
        env.put_file(
            "skills/a/SKILL.md",
            "---\nname: Bad_Name--\ndescription: Bad skill\n---\nContent",
        );

        let (skills, diagnostics) = load_skills(&env, &[arg("skills")]).await;

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "Bad_Name--");
        assert_eq!(
            diagnostics,
            vec![
                SkillDiagnostic {
                    code: SkillDiagnosticCode::InvalidMetadata,
                    message: "name \"Bad_Name--\" does not match parent directory \"a\""
                        .to_string(),
                    path: "/project/skills/a/SKILL.md".to_string(),
                },
                // Uppercase, underscore, trailing hyphen, consecutive hyphens.
                SkillDiagnostic {
                    code: SkillDiagnosticCode::InvalidMetadata,
                    message: "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".to_string(),
                    path: "/project/skills/a/SKILL.md".to_string(),
                },
                SkillDiagnostic {
                    code: SkillDiagnosticCode::InvalidMetadata,
                    message: "name must not start or end with a hyphen".to_string(),
                    path: "/project/skills/a/SKILL.md".to_string(),
                },
                SkillDiagnostic {
                    code: SkillDiagnosticCode::InvalidMetadata,
                    message: "name must not contain consecutive hyphens".to_string(),
                    path: "/project/skills/a/SKILL.md".to_string(),
                },
            ]
        );
    }

    /// YAML syntax errors in the frontmatter produce a `parse_failed`
    /// diagnostic and drop the skill (skills.ts:244-248).
    #[tokio::test]
    async fn test_load_skills_parse_failure_diagnostic() {
        let env = MemoryEnv::new("/project");
        env.put_dir("skills/broken");
        env.put_file(
            "skills/broken/SKILL.md",
            "---\nname: broken\ndescription: [unterminated\n---\nBody",
        );

        let (skills, diagnostics) = load_skills(&env, &[arg("skills")]).await;

        assert_eq!(skills, Vec::new());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, SkillDiagnosticCode::ParseFailed);
        assert_eq!(diagnostics[0].path, "/project/skills/broken/SKILL.md");
    }

    /// Upstream `formats skill invocations with additional instructions`
    /// (resource-formatting.test.ts:6).
    #[test]
    fn test_format_skill_invocation_with_instructions() {
        let skill = skill(
            "inspect",
            "Inspect things",
            "Use inspection tools.",
            "/project/.pi/skills/inspect/SKILL.md",
            None,
        );
        assert_eq!(
            format_skill_invocation(&skill, Some("Check errors.")),
            "<skill name=\"inspect\" location=\"/project/.pi/skills/inspect/SKILL.md\">\n\
             References are relative to /project/.pi/skills/inspect.\n\n\
             Use inspection tools.\n\
             </skill>\n\n\
             Check errors."
        );
    }

    /// Without instructions (or with an empty one — upstream truthiness) the
    /// skill block is returned alone.
    #[test]
    fn test_format_skill_invocation_without_instructions() {
        let skill = skill(
            "inspect",
            "Inspect things",
            "Use inspection tools.",
            "/s/SKILL.md",
            None,
        );
        assert_eq!(
            format_skill_invocation(&skill, None),
            "<skill name=\"inspect\" location=\"/s/SKILL.md\">\n\
             References are relative to /s.\n\n\
             Use inspection tools.\n\
             </skill>"
        );
        assert_eq!(
            format_skill_invocation(&skill, Some("")),
            format_skill_invocation(&skill, None)
        );
    }
}

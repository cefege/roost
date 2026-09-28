//! The unreached-module rule: every `.rs` file under a crate's `src/` must be
//! reachable from that crate's module roots. `cargo xtask lint` calls `run`;
//! it depends on `source_tree` for walking and on `violation` for reporting.
//!
//! A file that no `mod` line names is not compiled. It is not dead code that
//! rustc can warn about — it is text outside the program, so a reader greps it,
//! a reviewer reads it, a test in it can be parked in /tmp instead of
//! type-checked, and nothing anywhere reports the discrepancy. Seven capabilities
//! were found that way; this rule is the general form of the instrument.
//!
//! THE GRAPH IS TRANSITIVE, because the one-level case under-reports. A file
//! declared by a module that nothing reaches is itself unreachable, and naming
//! the child while ignoring the parent points at the wrong file.
//!
//! WHAT THIS DELIBERATELY IS NOT: a dead-code detector. It answers "is this file
//! in the module graph", not "is anything in it called". Symbol reachability is
//! the other half of that class and stays a `grep` a person runs — a rule that
//! tried to be both would produce false positives, and a lint that cries wolf
//! gets deleted, leaving the class with no instrument at all.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::source_tree;
use crate::violation::Violation;

const RULE: &str =
    "every .rs file under src/ must be reachable from a crate root via mod declarations";
const MEMORY: &str =
    "docs/FAILURE-INDEX.md — a capability that is implemented, tested, and never called";

/// Crate roots, by FILE NAME. Cargo compiles these without a `mod` line naming
/// them.
///
/// Compared against `file_name`, never `file_stem`: the stem of `lib.rs` is
/// `lib`, and matching a stem list against file names reaches nothing — which
/// reads as a clean tree rather than as a rule that checks nothing.
const ROOTS: [&str; 3] = ["lib.rs", "main.rs", "mod.rs"];

/// The files under one `src/`, and the subset the module graph reaches.
struct ModuleGraph {
    all: Vec<(PathBuf, String)>,
    reached: BTreeSet<PathBuf>,
}

/// A module the graph has not walked yet: the file, and the directory that
/// file's OWN submodules live in.
struct Pending {
    file: PathBuf,
    children_dir: PathBuf,
}

pub fn run() -> crate::ratchet::CheckOutcome {
    let mut violations = Vec::new();
    let mut checked = 0;
    for src in crate_src_dirs() {
        let Some(files) = graph_from(&src) else {
            continue;
        };
        for (path, _) in &files.all {
            checked += 1;
            if !files.reached.contains(path) {
                violations.push(Violation::new(
                    source_tree::repo_relative(path),
                    0,
                    "not reachable from any crate root: no `mod` line names it, so it is never compiled",
                    RULE,
                    MEMORY,
                ));
            }
        }
    }
    crate::ratchet::CheckOutcome {
        checked,
        violations,
    }
}

/// Every `crates/*/src` directory.
///
/// `source_tree::walk` yields FILES, so the directories are recovered from the
/// crate roots themselves rather than by filtering the walk for directories. A
/// walk filtered on `is_dir()` returns nothing at all, and a rule that finds no
/// files reports zero violations — indistinguishable from a clean tree.
fn crate_src_dirs() -> Vec<PathBuf> {
    let crates = source_tree::repo_root().join("crates");
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for path in source_tree::walk(&crates) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(parent) = path.parent() else {
            continue;
        };
        if ROOTS.contains(&name) && parent.file_name().is_some_and(|dir| dir == "src") {
            dirs.insert(parent.to_path_buf());
        }
    }
    dirs.into_iter().collect()
}

/// Breadth-first from the crate roots: each reached file contributes the modules
/// it declares, and each declared module is the next file to read.
fn graph_from(src: &Path) -> Option<ModuleGraph> {
    let all: Vec<(PathBuf, String)> = source_tree::walk(src)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|suffix| suffix == "rs"))
        // src/bin/ entries are auto-discovered binaries, not declared modules.
        .filter(|path| !path.to_string_lossy().contains("/src/bin/"))
        .map(|path| {
            let text = source_tree::read_text(&path).unwrap_or_default();
            (path, text)
        })
        .collect();
    if all.is_empty() {
        return None;
    }
    let mut reached: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue: VecDeque<Pending> = VecDeque::new();
    for (path, _) in &all {
        // DIRECTLY in `src/`, not merely NAMED mod.rs. A nested `dir/mod.rs` is
        // an ordinary module file reached by `mod dir;` — letting one seed
        // itself would make every unregistered subtree reachable, and the rule
        // would report nothing while looking like it had run.
        let is_root = path.parent() == Some(src)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| ROOTS.contains(&name));
        if is_root {
            reached.insert(path.clone());
            queue.push_back(Pending {
                file: path.clone(),
                children_dir: src.to_path_buf(),
            });
        }
    }
    while let Some(Pending { file, children_dir }) = queue.pop_front() {
        let Some((_, text)) = all.iter().find(|(path, _)| path == &file) else {
            continue;
        };
        for Declared { file, name } in declared_modules(&children_dir, text) {
            // The child's own children live in a directory named for the child's
            // MODULE NAME — not its file stem, which differ under #[path].
            let child_dir = children_dir.join(&name);
            if reached.insert(file.clone()) {
                queue.push_back(Pending {
                    file,
                    children_dir: child_dir,
                });
            }
        }
    }
    Some(ModuleGraph { all, reached })
}

/// A module `text` declares, and the name its own children are filed under.
struct Declared {
    file: PathBuf,
    name: String,
}

/// The modules `text` declares, resolved against `children_dir` — the directory
/// the DECLARING MODULE's own submodules live in.
///
/// That is deliberately not the declaring file's directory. Rust resolves a
/// submodule against its parent's module NAME, so `cell_row.rs` declaring
/// `pub mod dom;` means `cell_row/dom.rs`, not `dom.rs`. Resolving against the
/// file's own parent directory reports every such file as unreached — which is
/// what the first version of this rule did, and it named a healthy file on the
/// first run it was pointed at a real tree.
fn declared_modules(children_dir: &Path, text: &str) -> Vec<Declared> {
    let mut declared = Vec::new();
    let mut path_override: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(attribute) = path_attribute(trimmed) {
            path_override = Some(attribute);
            continue;
        }
        let Some(name) = module_name(trimmed) else {
            continue;
        };
        let file = match &path_override {
            Some(custom) => children_dir.join(custom),
            None => {
                let nested = children_dir.join(&name).join("mod.rs");
                if nested.is_file() {
                    nested
                } else {
                    children_dir.join(format!("{name}.rs"))
                }
            }
        };
        path_override = None;
        if file.is_file() {
            declared.push(Declared { file, name });
        }
    }
    declared
}

/// The `#[path = "…"]` string on this line, if it carries one.
fn path_attribute(line: &str) -> Option<String> {
    if !line.starts_with("#[path") {
        return None;
    }
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_owned())
}

/// The module name in a `mod name;` declaration, or `None` for an inline block,
/// a `use`, or a call to something that merely starts with `mod`.
fn module_name(line: &str) -> Option<String> {
    let mut rest = line.strip_prefix("pub ").unwrap_or(line);
    rest = rest.strip_prefix("pub(crate) ").unwrap_or(rest);
    rest = rest.strip_prefix('(').unwrap_or(rest);
    rest = rest.strip_prefix("unsafe ").unwrap_or(rest);
    let rest = rest.strip_prefix("mod ")?;
    let name: String = rest
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    // An inline block declares nothing on disk.
    if rest[name.len()..].trim_start().starts_with('{') {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crate whose `src/` holds exactly the files the test writes.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("roost-unreached-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("scratch crate");
        root.join("src")
    }

    fn write(directory: &Path, relative: &str, text: &str) {
        let path = directory.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write fixture");
    }

    /// THE RED CASE: real, complete code that no `mod` line reaches.
    #[test]
    fn a_subtree_nothing_names_is_reported() {
        let src = scratch("red");
        write(&src, "lib.rs", "pub mod registered;\n");
        write(&src, "registered.rs", "pub fn ok() {}\n");
        write(&src, "find/mod.rs", "pub mod hits;\n");
        write(&src, "find/hits.rs", "pub fn orphaned() {}\n");

        let graph = graph_from(&src).expect("a graph");
        assert!(graph.reached.contains(&src.join("registered.rs")));
        // BOTH are unreached: lib.rs never says `mod find;`. Naming the leaf
        // alone would point at the wrong file — the leaf is fine, its parent is
        // what is orphaned.
        for orphan in ["find/mod.rs", "find/hits.rs"] {
            assert!(
                !graph.reached.contains(&src.join(orphan)),
                "{orphan} should be unreached while lib.rs does not name find"
            );
        }
    }

    /// THE GREEN CASE: one `pub mod find;` reaches all of it. One line is the
    /// whole difference between a compiled file and text outside the program.
    #[test]
    fn one_mod_line_reaches_the_whole_subtree() {
        let src = scratch("green");
        write(&src, "lib.rs", "pub mod registered;\npub mod find;\n");
        write(&src, "registered.rs", "pub fn ok() {}\n");
        write(&src, "find/mod.rs", "pub mod hits;\n");
        write(&src, "find/hits.rs", "pub fn now_compiled() {}\n");

        let graph = graph_from(&src).expect("a graph");
        for relative in ["registered.rs", "find/mod.rs", "find/hits.rs"] {
            assert!(
                graph.reached.contains(&src.join(relative)),
                "{relative} should be reached once lib.rs names find"
            );
        }
    }

    /// A submodule of a `foo.rs` lives in `foo/`, NOT beside it. This is the
    /// case the first version got wrong, and it reported a healthy file on the
    /// real tree.
    #[test]
    fn a_submodule_of_a_file_lives_in_that_files_own_directory() {
        let src = scratch("sibling-dir");
        write(&src, "lib.rs", "pub mod cell_row;\n");
        write(&src, "cell_row.rs", "pub mod dom;\npub fn decide() {}\n");
        write(&src, "cell_row/dom.rs", "pub fn paint() {}\n");

        let graph = graph_from(&src).expect("a graph");
        assert!(
            graph.reached.contains(&src.join("cell_row/dom.rs")),
            "cell_row.rs names dom, and dom lives in cell_row/"
        );
    }

    #[test]
    fn a_crate_root_needs_no_mod_line() {
        let src = scratch("root");
        write(&src, "lib.rs", "pub fn root() {}\n");
        let graph = graph_from(&src).expect("a graph");
        assert!(graph.reached.contains(&src.join("lib.rs")));
    }

    #[test]
    fn an_inline_module_declares_nothing_on_disk() {
        assert_eq!(module_name("mod inline { pub fn a() {} }"), None);
        assert_eq!(module_name("mod declared;").as_deref(), Some("declared"));
        assert_eq!(module_name("pub mod public;").as_deref(), Some("public"));
    }

    #[test]
    fn a_use_or_a_call_is_not_a_declaration() {
        assert_eq!(module_name("use mod::thing;"), None);
        assert_eq!(module_name("let x = mod::thing();"), None);
        assert_eq!(module_name("modulate();"), None);
    }

    /// A `#[path]` module's NAME and its FILE differ, and its children are
    /// filed under the NAME — so the rule must carry the name, not the stem.
    #[test]
    fn a_path_attribute_moves_the_file_not_the_module_name() {
        let src = scratch("path");
        write(
            &src,
            "lib.rs",
            "#[path = \"oddly_named.rs\"]\npub mod conventional_name;\n",
        );
        write(&src, "oddly_named.rs", "pub mod inner;\n");
        write(&src, "conventional_name/inner.rs", "pub fn deep() {}\n");

        let graph = graph_from(&src).expect("a graph");
        assert!(graph.reached.contains(&src.join("oddly_named.rs")));
        assert!(
            graph
                .reached
                .contains(&src.join("conventional_name/inner.rs"))
        );
        assert_eq!(
            path_attribute("#[path = \"a.rs\"]").as_deref(),
            Some("a.rs")
        );
        assert_eq!(path_attribute("#[derive(Clone)]"), None);
    }

    /// `dir/mod.rs` and `dir.rs` are both conventional for `mod dir;`, and a rule
    /// that picked the wrong one would report a real module as unreached.
    #[test]
    fn both_conventional_module_layouts_resolve() {
        let src = scratch("layouts");
        write(&src, "lib.rs", "pub mod as_dir;\npub mod as_file;\n");
        write(&src, "as_dir/mod.rs", "");
        write(&src, "as_file.rs", "");

        let graph = graph_from(&src).expect("a graph");
        assert!(graph.reached.contains(&src.join("as_dir/mod.rs")));
        assert!(graph.reached.contains(&src.join("as_file.rs")));
    }
}

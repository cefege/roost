//! Cargo's own manifest grammar for lint tables, so a crate cannot hold a copy
//! of the workspace lint table and drift from it silently. A member that
//! inherits `workspace = true` needs no checking; a member that restates a lint
//! the workspace also defines has a COPY, and the copy stops receiving every
//! lint added after it was written. Cargo permits no way to mix the two, so a
//! crate that genuinely needs one override must copy, and that copy is this
//! rule's subject. Called from `xtask lint`; `xtask/` is not a track's to edit.

use std::collections::BTreeSet;
use std::path::Path;

use crate::source_tree;
use crate::violation::Violation;

const RULE: &str =
    "lints: a member that restates a workspace lint must be listed, so the copy cannot drift";
const MEMORY: &str = "Cargo.toml — [workspace.lints]";
/// Members permitted to hold a copy, and why cargo permits no alternative.
const COPY_EXEMPT: &[(&str, &str)] = &[(
    "roost-keeper",
    "needs `unsafe_code = \"allow\"` for a signal handler, and cargo REJECTS a manifest that \
     both inherits `workspace = true` and overrides a value, so copy-and-override is the only \
     form that parses. The cost is that a new workspace lint does not reach this crate until \
     someone adds it here, which is what this list is for.",
)];

/// The lint keys a lint-shaped table defines, and whether one of them inherits.
#[derive(Debug, Default, PartialEq, Eq)]
struct LintTable {
    inherits: bool,
    keys: BTreeSet<String>,
}

/// Read a manifest's lint tables.
///
/// The grammar is deliberately tiny: a lint-table header (see
/// [`is_lint_header`]) and, inside one of those, `key = value`. **This is a
/// scanner, not a TOML parser**, and the cost of that is written into the tests:
/// a form this grammar cannot see is silently not linted, and it reports nothing
/// looking exactly like a clean tree.
fn read_lint_table(manifest: &str) -> LintTable {
    let mut table = LintTable::default();
    let mut inside = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            inside = is_lint_header(line);
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        table.keys.insert(key.to_string());
        if key == "workspace" && value.trim() == "true" {
            table.inherits = true;
        }
    }
    table
}

/// Whether a table header names a lint table, in any of its legal spellings.
///
/// There are five, and the first version of this function knew three — which
/// made the entire rule dead, because the one that matters most is the root's
/// `[workspace.lints.rust]`, and a check that cannot match the legal form of
/// the thing it searches for reports nothing forever while looking exactly like
/// a passing gate. Both prefixes are pinned in the tests for that reason.
///
/// `[lints]`, `[lints.rust]` and `[lints.clippy]` are a MEMBER's tables;
/// `[workspace.lints.rust]` and `[workspace.lints.clippy]` are the root's. A
/// lint GROUP (`[lints.clippy.all]`) is legal cargo and defines keys the same
/// way, so it counts here too.
fn is_lint_header(line: &str) -> bool {
    let Some(rest) = line
        .strip_prefix("[workspace.lints")
        .or_else(|| line.strip_prefix("[lints"))
    else {
        return false;
    };
    rest == "]"
        || rest == ".rust]"
        || rest == ".clippy]"
        || (rest.starts_with('.') && rest.ends_with(']') && !rest.contains(' '))
}

pub fn run() -> crate::ratchet::CheckOutcome {
    let root = source_tree::repo_root().join("Cargo.toml");
    let workspace = read_lint_table(&read_manifest(&root));
    let mut violations = Vec::new();
    let mut checked = 0usize;

    for manifest in member_manifests() {
        let Some(name) = crate_name(&manifest) else {
            continue;
        };
        checked += 1;
        let local = read_lint_table(&read_manifest(&manifest));
        if local.inherits {
            continue;
        }
        let overlapping: Vec<&str> = local
            .keys
            .iter()
            .filter(|key| workspace.keys.contains(*key))
            .map(String::as_str)
            .collect();
        if overlapping.is_empty() || COPY_EXEMPT.iter().any(|(exempt, _)| *exempt == name) {
            continue;
        }
        violations.push(Violation::new(
            format!("crates/{name}/Cargo.toml"),
            0,
            format!(
                "restates [{}] without `workspace = true`, so it is a copy that stops receiving \
                 every lint added after it was written — add it to COPY_EXEMPT with the reason, \
                 or inherit",
                overlapping.join(", ")
            ),
            RULE,
            MEMORY,
        ));
    }
    crate::ratchet::CheckOutcome {
        checked,
        violations,
    }
}

/// Every workspace member manifest.
fn member_manifests() -> Vec<std::path::PathBuf> {
    let crates = source_tree::repo_root().join("crates");
    let Ok(entries) = std::fs::read_dir(&crates) else {
        return Vec::new();
    };
    let mut manifests: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|entry| entry.path().join("Cargo.toml"))
        .filter(|path| path.is_file())
        .collect();
    manifests.sort();
    manifests
}

/// The directory name, which is the crate name in this workspace.
fn crate_name(manifest: &Path) -> Option<String> {
    manifest
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().into_owned())
}

/// A missing manifest reads as empty, and empty is "no lint table", which is
/// the answer that produces no violation. A file the gate cannot read must not
/// be a file the gate passes.
fn read_manifest(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{LintTable, is_lint_header, read_lint_table};

    /// The inheritance form, which every member except `roost-keeper` uses.
    #[test]
    fn reads_an_inheriting_table() {
        let table = read_lint_table(
            r#"
[package]
name = "x"

[lints]
workspace = true
"#,
        );
        assert!(table.inherits);
        assert!(table.keys.contains("workspace"));
    }

    /// Every legal spelling, and BOTH prefixes. The workspace form is the one
    /// that made the rule dead: without it the comparison set was always empty,
    /// so no crate could ever overlap and the check passed on a tree where it
    /// should have failed.
    #[test]
    fn recognises_every_legal_lint_header() {
        for header in [
            "[lints]",
            "[lints.rust]",
            "[lints.clippy]",
            "[lints.clippy.all]",
            "[workspace.lints]",
            "[workspace.lints.rust]",
            "[workspace.lints.clippy]",
        ] {
            assert!(is_lint_header(header), "{header} is a legal lint table");
        }
        for header in [
            "[package]",
            "[dependencies]",
            "[lintsx]",
            "[lints .rust]",
            // A MEMBER's dependency-inheritance table is not a lint table, and
            // its `version.workspace = true` lines must not read as inheritance.
            "[workspace]",
        ] {
            assert!(!is_lint_header(header), "{header} is not a lint table");
        }
    }

    /// The workspace table is what every copy is compared against, so reading it
    /// is half the rule. Pinned directly against the shape this repo actually
    /// writes, because a test that only used a synthetic form would have passed
    /// while the rule was dead.
    #[test]
    fn reads_the_workspaces_own_table_shape() {
        let table = read_lint_table(
            r#"
[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
"#,
        );
        assert!(!table.inherits);
        let expected: BTreeSet<String> = [
            "expect_used",
            "missing_debug_implementations",
            "unsafe_code",
            "unwrap_used",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(table.keys, expected);
    }

    /// An overriding crate that does NOT inherit: this is the copy the rule is
    /// about, and its key set is what gets compared against the workspace's.
    #[test]
    fn reads_an_overriding_table_without_inheriting() {
        let table = read_lint_table(
            r#"
[lints.rust]
# a comment inside the table must not become a key
unsafe_code = "allow"
missing_debug_implementations = "warn"
"#,
        );
        assert!(!table.inherits);
        let expected: BTreeSet<String> = ["missing_debug_implementations", "unsafe_code"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(table.keys, expected);
    }

    /// A table header ends the previous table, so a key after the next section
    /// is not a lint key. Getting this wrong is how a dependency's
    /// `workspace = true` makes a crate look like it inherits the lints.
    #[test]
    fn a_key_after_the_next_header_is_not_a_lint() {
        let table = read_lint_table(
            r#"
[lints]
workspace = true

[dependencies]
serde.workspace = true
"#,
        );
        assert!(table.inherits);
        assert!(
            !table.keys.contains("serde.workspace"),
            "a dependency key is not a lint key: {:?}",
            table.keys
        );
    }

    /// The comparison the rule actually makes, stated as a test so a change to
    /// either side of it has to be deliberate.
    #[test]
    fn a_copy_overlaps_the_workspace_and_an_inheriting_manifest_does_not() {
        let workspace = read_lint_table("[workspace.lints.rust]\nunsafe_code = \"forbid\"\n");
        let copy = read_lint_table("[lints.rust]\nunsafe_code = \"allow\"\n");
        let inheriting = read_lint_table("[lints]\nworkspace = true\n");

        assert!(
            copy.keys.iter().any(|key| workspace.keys.contains(key)),
            "an overriding crate that restates a workspace lint is the defect"
        );
        assert!(
            !inheriting
                .keys
                .iter()
                .any(|key| workspace.keys.contains(key)),
            "an inheriting crate states no lint of its own, so there is nothing to drift"
        );
    }

    #[test]
    fn an_empty_manifest_is_the_empty_table() {
        assert_eq!(read_lint_table(""), LintTable::default());
    }
}

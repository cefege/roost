//! Tests module-graph reachability across visibility and path boundaries.
//! Called by xtask's unit-test harness.
//! Uses scratch Rust source trees and the parent graph walker.

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

/// A `#[path]` inside a NON-`mod.rs` child resolves against the FILE's own
/// directory, not the module-name directory.
///
/// The test above cannot see the difference, because its declaring file is a
/// crate root and the two directories coincide there. This one puts the
/// declaration in `voice/state.rs` with the file in `voice/tests/`, which is
/// the layout a module of its own test suite uses, and it is the case that
/// reported a compiling, passing, fully reachable test suite as "never
/// compiled".
#[test]
fn a_path_attribute_in_a_child_module_resolves_against_the_file() {
    let src = scratch("path-child");
    write(&src, "lib.rs", "pub mod voice;\n");
    write(&src, "voice.rs", "pub mod state;\n");
    write(
        &src,
        "voice/state.rs",
        "#[cfg(test)]\n#[path = \"tests/state_tests.rs\"]\nmod tests;\n",
    );
    write(&src, "voice/tests/state_tests.rs", "pub fn pinned() {}\n");

    let graph = graph_from(&src).expect("a graph");
    assert!(
        graph
            .reached
            .contains(&src.join("voice/tests/state_tests.rs")),
        "a test suite rustc compiles must not be reported as unreached"
    );
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

#[test]
fn restricted_visibility_reaches_declared_children_but_not_orphans() {
    let src = scratch("restricted-visibility");
    write(&src, "lib.rs", "mod scope;\n");
    write(
        &src,
        "scope.rs",
        "pub(in crate::scope) mod restricted;\npub(super) mod parent_visible;\npub(self) mod local;\npub(crate) mod crate_visible;\n",
    );
    for child in [
        "restricted",
        "parent_visible",
        "local",
        "crate_visible",
        "orphan",
    ] {
        write(&src, &format!("scope/{child}.rs"), "");
    }
    let graph = graph_from(&src).expect("module graph");
    let expected: BTreeSet<PathBuf> = [
        "lib.rs",
        "scope.rs",
        "scope/restricted.rs",
        "scope/parent_visible.rs",
        "scope/local.rs",
        "scope/crate_visible.rs",
    ]
    .into_iter()
    .map(|relative| src.join(relative))
    .collect();
    assert_eq!(graph.reached, expected);
}

//! The folder picker's "New folder" check: v2
//! `apps/web/tests/folderNameValidation.test.ts` over
//! `store::folder_name_validation`. (`folderActivity.test.ts` runs in
//! `roost-web/tests/worker_paths.rs`, against the browser's real path codec.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::store::folder_name_validation::validate_new_folder_name;

fn refused(name: &str, siblings: &[&str]) -> String {
    validate_new_folder_name(name, siblings).expect_err("refused")
}

#[test]
fn separators_blank_names_and_control_characters_are_named() {
    assert_eq!(refused("a/b", &[]), "Folder names can't contain / or \\.");
    assert_eq!(refused("a\\b", &[]), "Folder names can't contain / or \\.");
    assert_eq!(refused("", &[]), "Enter a folder name.");
    assert_eq!(refused("  ", &[]), "Enter a folder name.");
    assert_eq!(
        refused("na\u{7}me", &[]),
        "Folder names can't contain control characters."
    );
    assert_eq!(
        refused("na\tme", &[]),
        "Folder names can't contain control characters."
    );
}

#[test]
fn a_sibling_collision_ignores_case_and_quotes_the_trimmed_name() {
    assert_eq!(
        refused("Docs", &["docs"]),
        "A folder named \"Docs\" already exists here."
    );
    assert_eq!(
        refused("  Docs  ", &["DOCS", "src"]),
        "A folder named \"Docs\" already exists here."
    );
    assert_eq!(validate_new_folder_name("ok", &["other"]), Ok(()));
}

#[test]
fn dot_names_trailing_periods_and_length_are_refused_in_order() {
    assert_eq!(refused("..", &[]), "Choose a name other than . or ..");
    assert_eq!(refused(".", &[]), "Choose a name other than . or ..");
    assert_eq!(
        refused("build.", &[]),
        "Folder names can't end with a space or period."
    );
    assert_eq!(
        refused("docs .", &[]),
        "Folder names can't end with a space or period."
    );
    assert_eq!(validate_new_folder_name("folder  ", &[]), Ok(()));
    assert_eq!(validate_new_folder_name("my folder", &[]), Ok(()));
    assert_eq!(validate_new_folder_name(&"n".repeat(255), &[]), Ok(()));
    assert_eq!(
        refused(&"n".repeat(256), &[]),
        "Folder names must be 255 characters or fewer."
    );
}

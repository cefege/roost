//! The agent skill `roost skill` prints: the properties an agent depends on
//! before it acts, rather than its prose. The document is a markdown file in
//! the repository that ships verbatim, and an agent that reads a skill missing
//! its front matter treats the whole thing as instructions with no trigger and
//! no stated session requirement — so those two facts are asserted here.
//!
//! This is the whole test: the document's words are the document, and a test
//! that asserted a sentence would only assert that nobody edited it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_cli::ops::skill::skill_text;

#[test]
fn the_document_declares_its_name_and_when_to_use_it() {
    let text = skill_text();
    let front_matter = text
        .split_once("---")
        .and_then(|(_, rest)| rest.split_once("---"))
        .map(|(block, _)| block)
        .unwrap_or_else(|| panic!("the skill has no YAML front matter:\n{text}"));
    assert!(
        front_matter.contains("name: roost"),
        "front matter must name the skill"
    );
    assert!(
        front_matter.contains("description:"),
        "front matter must say when to use the skill, which is what an agent matches on"
    );
    // An agent that cannot prove it is inside a Roost session must stop rather
    // than guess an id, so the requirement is stated in the metadata as well as
    // in the body.
    assert!(
        front_matter.contains("ROOST_SESSION_ID"),
        "front matter must state the session requirement"
    );
}

#[test]
fn the_document_starts_by_proving_the_session_before_acting() {
    let text = skill_text();
    let establishing = text.find("ROOST_SESSION_ID").unwrap_or_default();
    let first_command = text.find("```sh").unwrap_or(text.len());
    assert!(
        establishing < first_command,
        "the session check must come before the first command the agent is told to run"
    );
}

//! What `roost api` answers when it is asked for something it does not have,
//! and how it reads the arguments of something it does. Owned by the L5 slice;
//! nothing here touches a coordinator, because both of these decisions are
//! made before a link is built.
//!
//! THE ASSERTION IS THE DECISION, NOT THE LIST. `assert!(!VERBS.is_empty())` is
//! satisfied by a table of one row, and it is the assertion a slice writes when
//! it has not decided anything. What is worth pinning is the refusal: an
//! unknown verb exits 2, not 1, and the failure names every verb that does
//! exist — because a wrapper keying its retry on 1 would otherwise loop forever
//! on a name that will never exist.

use clap::Parser;
use roost_cli::api::output::ApiOutput;
use roost_cli::api::verbs::VERBS;
use roost_cli::api::{ApiArgs, run_with};
use roost_cli::command_error::{GENERIC_FAILURE, REJECTED_INVOCATION};

/// A sink that keeps the two streams apart, which is the whole point of them
/// being apart.
#[derive(Debug, Default)]
struct Captured {
    answers: Vec<String>,
    progress: Vec<String>,
}

impl ApiOutput for Captured {
    fn answer(&mut self, line: &str) {
        self.answers.push(line.to_string());
    }

    fn progress(&mut self, line: &str) {
        self.progress.push(line.to_string());
    }
}

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[tokio::test]
async fn an_unknown_verb_exits_two_and_prints_every_verb_that_does_exist() {
    let mut captured = Captured::default();
    let failure = run_with(Some("frobnicate"), &[], &mut captured)
        .await
        .expect_err("this build does not answer that verb");

    assert_eq!(failure.code, REJECTED_INVOCATION);
    assert_ne!(
        failure.code, GENERIC_FAILURE,
        "a wrapper keyed on 1 would retry a name that will never exist"
    );
    for spec in VERBS {
        assert!(
            failure.message.contains(spec.usage),
            "the refusal for an unknown verb does not mention {}",
            spec.verb
        );
    }
    assert!(
        captured.answers.is_empty() && captured.progress.is_empty(),
        "a refusal this early must not have spoken: {:?} {:?}",
        captured.answers,
        captured.progress
    );
}

#[tokio::test]
async fn no_verb_at_all_is_the_same_refusal_rather_than_a_shrug() {
    let mut captured = Captured::default();
    let failure = run_with(None, &[], &mut captured)
        .await
        .expect_err("there is no default verb to run");

    assert_eq!(failure.code, REJECTED_INVOCATION);
    assert!(
        failure.message.contains("roost api agents"),
        "an operator who named no verb gets the list: {}",
        failure.message
    );
}

#[tokio::test]
async fn a_verb_refuses_the_arguments_it_does_not_take_before_it_dials_anything() {
    let mut captured = Captured::default();
    let failure = run_with(Some("agent-wait"), &argv(&["session-1"]), &mut captured)
        .await
        .expect_err("--until and --timeout are both required");

    assert_eq!(failure.code, REJECTED_INVOCATION);
    assert!(
        failure.message.contains("--until"),
        "the refusal names the option that is missing: {}",
        failure.message
    );
    assert!(
        captured.answers.is_empty() && captured.progress.is_empty(),
        "a refusal this early must not have spoken: {:?} {:?}",
        captured.answers,
        captured.progress
    );
}

#[tokio::test]
async fn every_registered_verb_has_a_usage_line_that_names_it() {
    for spec in VERBS {
        assert!(
            spec.usage.contains(&format!("roost api {}", spec.verb)),
            "{} has a usage line that does not name it: {}",
            spec.verb,
            spec.usage
        );
    }
}

#[test]
fn the_clap_parser_takes_a_verb_and_its_arguments_without_eating_the_arguments() {
    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        api: ApiArgs,
    }

    let parsed = Harness::try_parse_from([
        "roost",
        "agent-wait",
        "session-1",
        "--until",
        "working",
        "--timeout=30s",
    ])
    .expect("a trailing argument list carrying hyphens is what this subcommand is for");
    let ApiArgs { verb, args } = parsed.api;
    assert_eq!(verb.as_deref(), Some("agent-wait"));
    assert_eq!(
        args,
        argv(&["session-1", "--until", "working", "--timeout=30s"])
    );
}

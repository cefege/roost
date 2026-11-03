//! Slash completion: ranking, what Enter does, and the shared name column.
//! Compiled by slash_menu under cfg(test); drafts are the public input and
//! the registry in roost-protocol is the command table under test.

use super::{
    SlashAccept, SlashMenuState, SlashSources, accept_action, name_column_chars, slash_menu_state,
};

fn state(draft: &str) -> Option<SlashMenuState> {
    slash_menu_state(draft, &SlashSources::default())
}

fn accept_first(draft: &str) -> Option<SlashAccept> {
    state(draft).and_then(|state| state.options.first().map(accept_action))
}

#[test]
fn enter_runs_commands_that_need_nothing_more() {
    assert_eq!(
        accept_first("/us"),
        Some(SlashAccept::Submit("/usage".into()))
    );
    // Optional arguments: the bare command is a complete command.
    assert_eq!(
        accept_first("/pla"),
        Some(SlashAccept::Submit("/plan".into()))
    );
    // The browser answers the bare form itself.
    assert_eq!(
        accept_first("/mod"),
        Some(SlashAccept::Submit("/model".into()))
    );
    assert_eq!(
        accept_first("/advisor s"),
        Some(SlashAccept::Submit("/advisor status".into()))
    );
}

#[test]
fn enter_completes_a_command_that_cannot_run_without_an_argument() {
    assert_eq!(accept_first("/bt"), Some(SlashAccept::Fill("/btw ".into())));
    assert_eq!(
        accept_first("/eff"),
        Some(SlashAccept::Fill("/effort ".into()))
    );
}

#[test]
fn enter_completes_a_subcommand_that_needs_a_value_and_runs_one_that_does_not() {
    for (draft, filled) in [
        ("/modelpreset sa", "/modelpreset save "),
        ("/modelpreset sw", "/modelpreset switch "),
        ("/modelpreset d", "/modelpreset delete "),
    ] {
        assert_eq!(accept_first(draft), Some(SlashAccept::Fill(filled.into())));
        // Typing the value closes the menu, so the next Enter sends the command.
        assert_eq!(state(&format!("{filled}fast")), None);
    }
    assert_eq!(
        accept_first("/modelpreset l"),
        Some(SlashAccept::Submit("/modelpreset list".into()))
    );
}

#[test]
fn a_named_command_with_nothing_to_complete_offers_no_rows() {
    let named = state("/rename ");
    assert_eq!(named.as_ref().map(|state| state.options.len()), Some(0));
    assert_eq!(
        named.and_then(|state| state.argument_command).as_deref(),
        Some("rename")
    );
}

#[test]
fn omp_commands_come_from_the_transcript_catalog_and_plan_requires_a_mode() {
    let commands = vec![roost_protocol::wire::agent_chat::OmpSlashCommand {
        name: "review".to_owned(),
        description: "Review the current changes".to_owned(),
        input_hint: Some("focus".to_owned()),
    }];
    let sources = SlashSources::default();
    let Some(menu) = super::slash_menu_state_for_omp("/re", &sources, Some(&[]), &commands) else {
        panic!("OMP did not offer its discovered command");
    };
    assert_eq!(menu.options[0].label, "review");
    assert_eq!(menu.options[0].text, "/review");
    assert!(super::slash_menu_state_for_omp("/plan", &sources, Some(&[]), &[]).is_none());
    assert_eq!(
        super::slash_menu_state_for_omp("/plan", &sources, Some(&["plan".to_owned()]), &[])
            .map(|menu| menu.options[0].text.clone()),
        Some("/plan".to_owned())
    );
}

#[test]
fn omp_argument_separators_open_live_choices_without_a_typed_argument() {
    let sources = SlashSources {
        models: vec![("machine/live-model".into(), "Live model".into())],
        effort_levels: vec!["high".into()],
    };
    for (draft, expected) in [
        ("/model ", "/model machine/live-model"),
        ("/effort ", "/effort high"),
    ] {
        let accepted = super::slash_menu_state_for_omp(draft, &sources, Some(&[]), &[])
            .and_then(|state| state.options.first().map(accept_action));
        assert_eq!(accepted, Some(SlashAccept::Submit(expected.into())));
    }
}

#[test]
fn omp_plan_completion_never_submits_a_repeated_mode_argument() {
    let modes = ["default".to_owned(), "plan".to_owned()];
    for draft in ["/", "/pl", "/plan"] {
        let accepted =
            super::slash_menu_state_for_omp(draft, &SlashSources::default(), Some(&modes), &[])
                .and_then(|state| {
                    state
                        .options
                        .iter()
                        .find(|option| option.command_name == "plan")
                        .map(accept_action)
                });
        assert_eq!(accepted, Some(SlashAccept::Submit("/plan".into())));
    }
}

#[test]
fn omp_discovery_excludes_the_native_registry_and_keeps_optional_arguments_optional() {
    let commands = [roost_protocol::wire::agent_chat::OmpSlashCommand {
        name: "compact".into(),
        description: "Compact OMP context".into(),
        input_hint: Some("[instructions]".into()),
    }];
    let sources = SlashSources::default();
    assert!(super::slash_menu_state_for_omp("/settings", &sources, Some(&[]), &commands).is_none());
    let accepted = super::slash_menu_state_for_omp("/compact", &sources, Some(&[]), &commands)
        .and_then(|state| state.options.first().map(accept_action));
    assert_eq!(accepted, Some(SlashAccept::Submit("/compact".into())));
}

#[test]
fn long_completion_labels_cannot_push_the_summary_column_past_its_limit() {
    let mut short = state("/new").map(|state| state.options).unwrap_or_default();
    assert_eq!(name_column_chars(&short), 12);
    if let Some(option) = short.first_mut() {
        option.label = "provider/a-model-id-well-past-the-column-cap".into();
    }
    assert_eq!(name_column_chars(&short), 32);
}

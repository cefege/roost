//! Slash completion: ranking, what Enter does, and the shared name column.
//! Compiled by slash_menu under cfg(test); drafts are the public input and
//! the registry in roost-protocol is the command table under test.

use super::{
    SlashAccept, SlashMenuState, SlashSources, accept_action, name_column_chars, slash_menu_state,
};

fn state(draft: &str) -> Option<SlashMenuState> {
    slash_menu_state(draft, &SlashSources::default())
}

fn labels(draft: &str) -> Vec<String> {
    state(draft)
        .map(|state| {
            state
                .options
                .into_iter()
                .map(|option| option.label)
                .collect()
        })
        .unwrap_or_default()
}

fn accept_first(draft: &str) -> Option<SlashAccept> {
    state(draft).and_then(|state| state.options.first().map(accept_action))
}

#[test]
fn name_prefix_matches_rank_before_letters_found_inside_a_name() {
    assert_eq!(labels("/m"), ["model", "modelpreset", "compact", "rename"]);
}

#[test]
fn an_exact_name_ranks_first_and_a_bare_slash_lists_the_registry() {
    assert_eq!(labels("/plan").first().map(String::as_str), Some("plan"));
    assert_eq!(labels("/").first().map(String::as_str), Some("goal"));
}

#[test]
fn name_rows_carry_usage_and_argument_rows_do_not() {
    let names = state("/goal")
        .map(|state| state.options)
        .unwrap_or_default();
    assert_eq!(
        names.first().map(|option| option.usage),
        Some("[objective|command]")
    );
    let arguments = state("/goal ")
        .map(|state| state.options)
        .unwrap_or_default();
    assert!(arguments.iter().all(|option| option.usage.is_empty()));
    assert_eq!(
        arguments.first().map(|option| option.label.as_str()),
        Some("set")
    );
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
        named
            .and_then(|state| state.argument_command)
            .map(|command| command.name),
        Some("rename")
    );
}

#[test]
fn the_name_column_fits_the_widest_label_within_bounds() {
    let short = state("/new").map(|state| state.options).unwrap_or_default();
    assert_eq!(name_column_chars(&short), 12);
    let all = state("/").map(|state| state.options).unwrap_or_default();
    // "modelpreset" is the longest name: eleven characters plus the gap.
    assert_eq!(name_column_chars(&all), 13);
    let mut long = all;
    if let Some(first) = long.first_mut() {
        first.label = "provider/a-model-id-well-past-the-column-cap".into();
    }
    assert_eq!(name_column_chars(&long), 32);
}

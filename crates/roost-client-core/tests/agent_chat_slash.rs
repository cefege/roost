//! Shared slash-command parsing keeps client interception and coordinator
//! dispatch on the same registered command names and argument boundary.
//! These cases pin the distinction between commands and path-like text.

use roost_protocol::wire::agent_chat::parse_slash_command;

#[test]
fn registered_commands_and_aliases_parse_arguments() {
    let (command, args) = parse_slash_command("/usage").expect("known command");
    assert_eq!(command.name, "usage");
    assert_eq!(args, "");

    let (command, args) = parse_slash_command("/models anthropic/claude").expect("alias");
    assert_eq!(command.name, "model");
    assert_eq!(args, "anthropic/claude");
}

#[test]
fn unknown_commands_and_paths_remain_ordinary_text() {
    assert!(parse_slash_command("/foo").is_none());
    assert!(parse_slash_command("/etc/hosts").is_none());
}

#[test]
fn arguments_are_split_after_the_command_token() {
    let (command, args) =
        parse_slash_command("/plan add a test\nthen verify it").expect("plan command");
    assert_eq!(command.name, "plan");
    assert_eq!(args, "add a test\nthen verify it");
}

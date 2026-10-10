//! Shared slash-command names and parsing for the agent composer and runtime.
//! Client-only commands are marked so the browser can intercept their bare form.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub summary: &'static str,
    pub client_bare: bool,
}

pub const AGENT_SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "plan",
        aliases: &[],
        args: "[prompt]",
        summary: "Toggle plan mode",
        client_bare: false,
    },
    SlashCommand {
        name: "model",
        aliases: &["models"],
        args: "<selector>",
        summary: "Choose a model",
        client_bare: true,
    },
    SlashCommand {
        name: "effort",
        aliases: &[],
        args: "<level|auto>",
        summary: "Set thinking level",
        client_bare: false,
    },
    SlashCommand {
        name: "usage",
        aliases: &[],
        args: "",
        summary: "Show account usage",
        client_bare: false,
    },
    SlashCommand {
        name: "compact",
        aliases: &[],
        args: "[instructions]",
        summary: "Compact conversation history",
        client_bare: false,
    },
    SlashCommand {
        name: "login",
        aliases: &[],
        args: "[provider]",
        summary: "Connect a provider",
        client_bare: true,
    },
    SlashCommand {
        name: "logout",
        aliases: &[],
        args: "[provider]",
        summary: "Manage connected accounts",
        client_bare: true,
    },
    SlashCommand {
        name: "new",
        aliases: &[],
        args: "",
        summary: "Start a conversation",
        client_bare: true,
    },
    SlashCommand {
        name: "help",
        aliases: &[],
        args: "",
        summary: "List commands",
        client_bare: false,
    },
    SlashCommand {
        name: "advisor",
        aliases: &[],
        args: "[on|off|status]",
        summary: "Toggle turn reviews",
        client_bare: false,
    },
];

pub fn parse_slash_command(text: &str) -> Option<(&'static SlashCommand, &str)> {
    let trimmed = text.trim_start();
    let (token, args) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    let name = token.strip_prefix('/')?;
    if name.is_empty()
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    AGENT_SLASH_COMMANDS
        .iter()
        .find(|command| command.name == name || command.aliases.contains(&name))
        .map(|command| (command, args.trim()))
}

#[cfg(test)]
mod tests {
    use super::parse_slash_command;

    #[test]
    fn parses_registered_commands_aliases_and_arguments() {
        assert_eq!(
            parse_slash_command("/usage").map(|(command, _)| command.name),
            Some("usage")
        );
        assert_eq!(
            parse_slash_command("/models @smol").map(|(command, args)| (command.name, args)),
            Some(("model", "@smol"))
        );
        assert!(parse_slash_command("/foo").is_none());
        assert!(parse_slash_command("/etc/hosts").is_none());
    }
}

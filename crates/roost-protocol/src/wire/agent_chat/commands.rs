//! Shared slash-command names, argument metadata and parsing for the agent
//! composer and runtime. Client-only commands are marked so the browser can
//! intercept their bare form.

/// One fixed value a command accepts as its first argument, such as `on` for
/// `/advisor on`. The composer offers these as completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashArgument {
    pub name: &'static str,
    pub summary: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static str,
    pub summary: &'static str,
    pub client_bare: bool,
    /// Fixed first-argument values; empty when the argument is free text or
    /// comes from the active model (`/effort`).
    pub subcommands: &'static [SlashArgument],
    /// Complete example invocations shown in the composer's command help.
    pub examples: &'static [&'static str],
}

impl SlashCommand {
    /// Whether the command cannot run without an argument: `args` written as
    /// `<…>` is required, `[…]` is optional.
    pub fn requires_argument(&self) -> bool {
        self.args.starts_with('<')
    }
}

const ADVISOR_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "on",
        summary: "Turn the advisor on",
    },
    SlashArgument {
        name: "off",
        summary: "Turn the advisor off",
    },
    SlashArgument {
        name: "status",
        summary: "Show advisor state, reviews and cost",
    },
];

const LOOP_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "off",
        summary: "Stop the loop",
    },
    SlashArgument {
        name: "status",
        summary: "Show loop state",
    },
];

const GOAL_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "set",
        summary: "Set or replace the objective",
    },
    SlashArgument {
        name: "show",
        summary: "Show goal status",
    },
    SlashArgument {
        name: "pause",
        summary: "Pause autonomous work",
    },
    SlashArgument {
        name: "resume",
        summary: "Resume the goal",
    },
    SlashArgument {
        name: "drop",
        summary: "Drop the goal",
    },
    SlashArgument {
        name: "budget",
        summary: "Set a token budget or off",
    },
];

const MODELPRESET_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "list",
        summary: "List saved presets",
    },
    SlashArgument {
        name: "save",
        summary: "Save the all-folders roles under a name",
    },
    SlashArgument {
        name: "switch",
        summary: "Make a preset the all-folders roles",
    },
    SlashArgument {
        name: "delete",
        summary: "Delete a preset",
    },
];

pub const AGENT_SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "goal",
        aliases: &[],
        args: "[objective|command]",
        summary: "Work toward a durable objective",
        client_bare: false,
        subcommands: GOAL_SUBCOMMANDS,
        examples: &[
            "/goal ship the settings page",
            "/goal show",
            "/goal budget 50000",
        ],
    },
    SlashCommand {
        name: "plan",
        aliases: &[],
        args: "[prompt]",
        summary: "Toggle plan mode",
        client_bare: false,
        subcommands: &[],
        examples: &["/plan", "/plan map the auth code first"],
    },
    SlashCommand {
        name: "model",
        aliases: &["models"],
        args: "<selector>",
        summary: "Open the models hub, or switch this chat to a model",
        client_bare: true,
        subcommands: &[],
        examples: &["/model", "/model sonnet", "/model @smol"],
    },
    SlashCommand {
        name: "switch",
        aliases: &[],
        args: "<selector>",
        summary: "Switch this chat's model: a name, provider/id, @role, with an optional :level",
        client_bare: true,
        subcommands: &[],
        examples: &[
            "/switch",
            "/switch sonnet",
            "/switch opus:high",
            "/switch @smol",
        ],
    },
    SlashCommand {
        name: "modelpreset",
        aliases: &[],
        args: "[list|save|switch|delete] [name]",
        summary: "Save and switch named sets of model roles",
        client_bare: false,
        subcommands: MODELPRESET_SUBCOMMANDS,
        examples: &[
            "/modelpreset",
            "/modelpreset save fast",
            "/modelpreset switch fast",
        ],
    },
    SlashCommand {
        name: "effort",
        aliases: &[],
        args: "<level|auto>",
        summary: "Set thinking level",
        client_bare: false,
        subcommands: &[],
        examples: &["/effort high", "/effort auto"],
    },
    SlashCommand {
        name: "usage",
        aliases: &[],
        args: "",
        summary: "Show account usage",
        client_bare: false,
        subcommands: &[],
        examples: &["/usage"],
    },
    SlashCommand {
        name: "compact",
        aliases: &[],
        args: "[instructions]",
        summary: "Compact conversation history",
        client_bare: false,
        subcommands: &[],
        examples: &["/compact", "/compact keep the open questions"],
    },
    SlashCommand {
        name: "login",
        aliases: &[],
        args: "[provider]",
        summary: "Connect a provider",
        client_bare: true,
        subcommands: &[],
        examples: &["/login", "/login anthropic"],
    },
    SlashCommand {
        name: "logout",
        aliases: &[],
        args: "[provider]",
        summary: "Manage connected accounts",
        client_bare: true,
        subcommands: &[],
        examples: &["/logout"],
    },
    SlashCommand {
        name: "new",
        aliases: &[],
        args: "",
        summary: "Start a conversation",
        client_bare: true,
        subcommands: &[],
        examples: &["/new"],
    },
    SlashCommand {
        name: "settings",
        aliases: &[],
        args: "",
        summary: "Open settings",
        client_bare: true,
        subcommands: &[],
        examples: &["/settings"],
    },
    SlashCommand {
        name: "help",
        aliases: &[],
        args: "",
        summary: "List commands",
        client_bare: false,
        subcommands: &[],
        examples: &["/help"],
    },
    SlashCommand {
        name: "advisor",
        aliases: &[],
        args: "[on|off|status]",
        summary: "Toggle turn reviews",
        client_bare: false,
        subcommands: ADVISOR_SUBCOMMANDS,
        examples: &["/advisor", "/advisor status"],
    },
    SlashCommand {
        name: "btw",
        aliases: &[],
        args: "<question>",
        summary: "Ask a side question without adding it to the conversation",
        client_bare: false,
        subcommands: &[],
        examples: &["/btw what does retry_after return?"],
    },
    SlashCommand {
        name: "retry",
        aliases: &[],
        args: "",
        summary: "Discard the last answer and run the last message again",
        client_bare: false,
        subcommands: &[],
        examples: &["/retry"],
    },
    SlashCommand {
        name: "rewind",
        aliases: &["undo"],
        args: "",
        summary: "Undo the last turn, restoring the files it changed",
        client_bare: false,
        subcommands: &[],
        examples: &["/rewind"],
    },
    SlashCommand {
        name: "fork",
        aliases: &[],
        args: "",
        summary: "Copy this conversation into a new one",
        client_bare: false,
        subcommands: &[],
        examples: &["/fork"],
    },
    SlashCommand {
        name: "rename",
        aliases: &[],
        args: "<title>",
        summary: "Rename this conversation",
        client_bare: false,
        subcommands: &[],
        examples: &["/rename Login flow refactor"],
    },
    SlashCommand {
        name: "export",
        aliases: &[],
        args: "",
        summary: "Download this conversation as Markdown",
        client_bare: true,
        subcommands: &[],
        examples: &["/export"],
    },
    SlashCommand {
        name: "loop",
        aliases: &[],
        args: "[count|duration] [--while|--until '<cmd>'] [prompt]",
        summary: "Repeat your next prompt after each reply until a limit or Stop",
        client_bare: false,
        subcommands: LOOP_SUBCOMMANDS,
        examples: &[
            "/loop 10",
            "/loop 10m",
            "/loop 20 --until 'bun test' fix the failing tests",
            "/loop off",
        ],
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

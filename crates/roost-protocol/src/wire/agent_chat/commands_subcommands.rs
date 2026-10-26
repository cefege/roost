//! Fixed first-argument tables for `AGENT_SLASH_COMMANDS` entries. Split out
//! of `commands.rs` to keep it under the file-size cap; purely data.

use super::SlashArgument;

pub(super) const ADVISOR_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "on",
        summary: "Turn the advisor on",
        takes_value: false,
    },
    SlashArgument {
        name: "off",
        summary: "Turn the advisor off",
        takes_value: false,
    },
    SlashArgument {
        name: "status",
        summary: "Show advisor state, reviews and cost",
        takes_value: false,
    },
];

pub(super) const LOOP_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "off",
        summary: "Stop the loop",
        takes_value: false,
    },
    SlashArgument {
        name: "status",
        summary: "Show loop state",
        takes_value: false,
    },
];

pub(super) const GOAL_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "set",
        summary: "Set or replace the objective",
        takes_value: false,
    },
    SlashArgument {
        name: "show",
        summary: "Show goal status",
        takes_value: false,
    },
    SlashArgument {
        name: "pause",
        summary: "Pause autonomous work",
        takes_value: false,
    },
    SlashArgument {
        name: "resume",
        summary: "Resume the goal",
        takes_value: false,
    },
    SlashArgument {
        name: "drop",
        summary: "Drop the goal",
        takes_value: false,
    },
    SlashArgument {
        name: "budget",
        summary: "Set a token budget or off",
        takes_value: false,
    },
];

pub(super) const MODELPRESET_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "list",
        summary: "List saved presets",
        takes_value: false,
    },
    SlashArgument {
        name: "save",
        summary: "Save the all-folders roles under a name",
        takes_value: true,
    },
    SlashArgument {
        name: "switch",
        summary: "Make a preset the all-folders roles",
        takes_value: true,
    },
    SlashArgument {
        name: "delete",
        summary: "Delete a preset",
        takes_value: true,
    },
];

pub(super) const SCHEDULE_SUBCOMMANDS: &[SlashArgument] = &[
    SlashArgument {
        name: "in",
        summary: "Send once after a duration",
        takes_value: true,
    },
    SlashArgument {
        name: "every",
        summary: "Send on a repeating interval",
        takes_value: true,
    },
    SlashArgument {
        name: "at",
        summary: "Send once at an absolute time",
        takes_value: true,
    },
    SlashArgument {
        name: "cancel",
        summary: "Cancel a scheduled message",
        takes_value: true,
    },
];

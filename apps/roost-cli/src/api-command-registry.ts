// Canonical metadata for the small `roost api` surface documented by the
// release-matched ROOST skill. Tests recognize every skill example from this
// registry; guarded-prompt syntax stays owned by api-agent-prompt.ts.

import { AGENT_PROMPT_API_COMMAND } from "./api-agent-prompt.ts";

export interface ApiOptionDefinition {
  readonly takesValue: boolean;
  readonly required: boolean;
}

export interface DeclarativeApiCommandDefinition {
  readonly kind: "declarative";
  readonly verb: string;
  readonly usage: string;
  readonly positionalArgs: {
    readonly minimum: number;
    readonly maximum: number;
  };
  readonly options: Readonly<Record<string, ApiOptionDefinition | undefined>>;
}

export type RegisteredApiCommandDefinition =
  | DeclarativeApiCommandDefinition
  | typeof AGENT_PROMPT_API_COMMAND;

const AGENT_STATUS_API_COMMAND = {
  kind: "declarative",
  verb: "agent-status",
  usage: "roost api agent-status <session> [--json]",
  positionalArgs: { minimum: 1, maximum: 1 },
  options: {
    "--json": { takesValue: false, required: false },
  },
} as const satisfies DeclarativeApiCommandDefinition;

const AGENTS_API_COMMAND = {
  kind: "declarative",
  verb: "agents",
  usage: "roost api agents [--json]",
  positionalArgs: { minimum: 0, maximum: 0 },
  options: {
    "--json": { takesValue: false, required: false },
  },
} as const satisfies DeclarativeApiCommandDefinition;

const AGENT_WAIT_API_COMMAND = {
  kind: "declarative",
  verb: "agent-wait",
  usage: "roost api agent-wait <session> --until <states> --timeout <duration>",
  positionalArgs: { minimum: 1, maximum: 1 },
  options: {
    "--until": { takesValue: true, required: true },
    "--timeout": { takesValue: true, required: true },
  },
} as const satisfies DeclarativeApiCommandDefinition;

export const API_COMMAND_REGISTRY: Readonly<
  Record<string, RegisteredApiCommandDefinition | undefined>
> = Object.freeze({
  "agent-status": AGENT_STATUS_API_COMMAND,
  agents: AGENTS_API_COMMAND,
  "agent-wait": AGENT_WAIT_API_COMMAND,
  "agent-prompt": AGENT_PROMPT_API_COMMAND,
});


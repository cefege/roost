// Names of the worker environment entries that travel through an installed
// service definition (LaunchAgent plist / systemd --user unit). Read by the
// roost-cli deploy env composer and by the worker that consumes them, so both
// sides address the same installed entry. Deliberately dependency-free: the
// worker imports these names on host layouts that do not exist yet, so
// importing a name must never resolve a path or read a platform.

/** One-shot authorization for retiring a keeper the deployed worker can
 * neither adopt nor prove empty. Spent by the activation that receives it. */
export const KEEPER_FORCE_LIVE_RETIRE_ENV = "ROOST_KEEPER_FORCE_LIVE_RETIRE";

/** Opt-in for restoring an agent conversation on respawn. */
export const AGENT_CONVERSATION_RESTORE_ENV = "ROOST_AGENT_CONVERSATION_RESTORE";

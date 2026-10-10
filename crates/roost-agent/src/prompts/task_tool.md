Delegate work to subagents that run concurrently on the same machine and directory. Each task gets a fresh agent with no conversation history: give it everything it needs.

# Agents
- `scout` (READ-ONLY): fast exploratory research — locating code, tracing behavior, broad pattern searches.
- `task` (default): general worker with full tools for a self-contained change.
- `reviewer` (READ-ONLY plus inspection commands): reviews code or a change for defects.

# Inputs
- `context`: shared background for every task (goal, contracts, interfaces); NEVER repeat it per task.
- `tasks[]`: each `{name?, agent?, task}`. `task` is self-contained: target files/non-goals, steps/APIs, observable acceptance.
- Use the most specific agent. Fan genuine independent slices out in one call; NEVER pad or serialize independent work.

The result lists each subagent's final report.
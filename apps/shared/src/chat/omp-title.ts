// omp identity from its OSC window title — the ONE definition, because the
// worker (chat-watcher gate) and the web (chat-toggle gate) both need it and
// had drifted copies.
//
// omp brands the title `π` and encodes RUN STATE in the separator slot
// (packages/coding-agent/src/utils/title-generator.ts::buildTerminalTitleWithState):
//
//   idle        π > label     — user's turn
//   working     π ⠋ label     — a Braille spinner frame, re-emitted per tick
//   attention   π ! label     — agent blocked on the user
//   state off   π: label      — the pre-state layout (tui.titleState=false)
//
// Both copies matched only `π >` and `π:`, so the moment the agent started
// working the title became `π ⠋ …`, identity went false, and the chat toggle
// disappeared — returning at idle. It also vanished on `π !`, i.e. exactly when
// the agent was waiting for the user.
//
// The separator set is `>`, `!`, `:` or ANY Braille pattern (U+2800–U+28FF), so
// a future spinner alphabet cannot resurrect this bug. Crucially `-` is NOT in
// the set: pi titles itself `π - <dir>` and must never be mistaken for omp.

const OMP_TITLE_RE = /^\u03C0(?::|[ \t](?:[>!]|[\u2800-\u28FF]))/;

/** True when an OSC title identifies an omp session, in ANY run state. */
export function isOmpTitle(title: string | undefined | null): boolean {
	return typeof title === "string" && OMP_TITLE_RE.test(title);
}

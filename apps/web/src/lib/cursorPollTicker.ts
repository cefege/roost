// Shared 500ms cursor-poll ticker — one interval for ALL mounted panes (was one
// per open session; the deck keeps every open session mounted). The ticker
// starts on first register and stops when the last pane unregisters.
// Per-instance gating (inLayout/visible/changed) stays in each callback.

const CURSOR_POLL_MS = 500;
const _cursorPollCbs = new Set<() => void>();
let _cursorPollHandle: number | null = null;

export function registerCursorPoll(cb: () => void): () => void {
	_cursorPollCbs.add(cb);
	if (_cursorPollHandle === null) {
		_cursorPollHandle = window.setInterval(() => {
			for (const f of _cursorPollCbs) f();
		}, CURSOR_POLL_MS);
	}
	return () => {
		_cursorPollCbs.delete(cb);
		if (_cursorPollCbs.size === 0 && _cursorPollHandle !== null) {
			clearInterval(_cursorPollHandle);
			_cursorPollHandle = null;
		}
	};
}

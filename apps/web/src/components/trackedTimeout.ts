// One-shot setTimeout whose lifetime is owned by the reactive scope that
// created the tracker.
//
// Untracked one-shot timers are the standard setState-after-unmount bug in
// this tree: a timeout fires after the component disposed and writes a signal
// nobody reads anymore (or worse, keeps an overlay alive). Calling plain
// setTimeout inside event handlers / async continuations has no reactive owner,
// so an onCleanup() wrapper must be bound at SETUP time — hence this factory:
// call it once per component, use the returned function anywhere below it.

import { onCleanup } from "solid-js";

type TimerHandle = ReturnType<typeof setTimeout>;

/** Create a dispose-bound setTimeout. Every timer started through the returned
 *  function is cleared when the owning component unmounts; a fired timer is
 *  dropped from the set so disposal stays cheap. */
export function createTrackedTimeouts(): (fn: () => void, ms: number) => TimerHandle {
	const pending = new Set<TimerHandle>();
	onCleanup(() => {
		for (const handle of pending) clearTimeout(handle);
		pending.clear();
	});
	return (fn, ms) => {
		const handle = setTimeout(() => {
			pending.delete(handle);
			fn();
		}, ms);
		pending.add(handle);
		return handle;
	};
}

// Cooperative scheduling primitives for terminal content search.
// The scanner yields between row slices and races terminal-control settling
// against both its page deadline and latest-query cancellation.

export function terminalSearchEventLoopYield(): Promise<void> {
	const { promise, resolve } = Promise.withResolvers<void>();
	setImmediate(resolve);
	return promise;
}

export async function terminalControlSettlesBeforeSearchDeadline(
	settled: Promise<void>,
	remainingMs: number,
	signal: AbortSignal,
): Promise<boolean> {
	if (remainingMs <= 0 || signal.aborted) return false;
	const { promise: timeout, resolve: resolveTimeout } = Promise.withResolvers<boolean>();
	const { promise: aborted, resolve: resolveAborted } = Promise.withResolvers<boolean>();
	const timer = setTimeout(() => { resolveTimeout(false); }, remainingMs);
	const onAbort = () => { resolveAborted(false); };
	signal.addEventListener("abort", onAbort, { once: true });
	try {
		return await Promise.race([settled.then(() => true), timeout, aborted]);
	} finally {
		clearTimeout(timer);
		signal.removeEventListener("abort", onAbort);
	}
}

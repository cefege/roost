// Fences SessionManager keeper-channel creation against keeper replacement.
// Every spawn or respawn owns one synchronous lease; update preparation closes
// admission before awaiting all leases that were already in flight. The same
// preparation state also freezes terminal writes, because a keeper about to be
// replaced must not absorb one more byte.

export interface SessionChannelCreationLease {
	release(): void;
}

export class SessionChannelCreationGate {
	#activeCreations = 0;
	#preparationCount = 0;
	readonly #drainWaiters = new Set<() => void>();

	get preparationActive(): boolean {
		return this.#preparationCount > 0;
	}

	/** Keeper replacement is imminent while preparation is open, so a terminal
	 * write must fail before it reaches the keeper the update is about to
	 * replace: a pre-write refusal is retryable, an absorbed write is not. */
	blocksTerminalWrites(): boolean {
		return this.preparationActive;
	}

	tryAcquire(): SessionChannelCreationLease | null {
		if (this.preparationActive) return null;
		this.#activeCreations += 1;
		let released = false;
		return {
			release: () => {
				if (released) return;
				released = true;
				this.#activeCreations -= 1;
				if (this.#activeCreations !== 0) return;
				for (const resolve of this.#drainWaiters) resolve();
				this.#drainWaiters.clear();
			},
		};
	}

	async beginPreparation(): Promise<() => void> {
		this.#preparationCount += 1;
		if (this.#activeCreations > 0) {
			await new Promise<void>((resolve) => {
				this.#drainWaiters.add(resolve);
			});
		}
		let rolledBack = false;
		return () => {
			if (rolledBack) return;
			rolledBack = true;
			this.#preparationCount -= 1;
		};
	}
}

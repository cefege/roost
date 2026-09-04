// deepgramKey — page-session cache for the configured Deepgram key returned to
// an authenticated dashboard-admin browser. It is the stored key (expiresIn: 0),
// not a temporary grant; caching avoids a WAN round-trip on every mic tap.
// Invalidation is generation-scoped so logout, key rotation, or credential
// rejection cannot let an older in-flight response repopulate the cache.
// Callers: MobileVoiceInput, TranscriptionPane, and managed logout.

import { coordClient } from "../connect.ts";

let cached: string | null = null;
let inflight: Promise<string> | null = null;
let generation = 0;

/** The cached key, the in-flight fetch, or a new fetch. Rejections propagate
 *  and leave the cache empty, so the next call retries. */
export function getDeepgramKey(): Promise<string> {
	if (cached !== null) return Promise.resolve(cached);
	if (inflight) return inflight;
	const requestGeneration = generation;
	let request: Promise<string>;
	request = coordClient
		.transcriptionGrantToken({})
		.then((r) => {
			if (requestGeneration !== generation) {
				throw new Error("Deepgram credential request was invalidated");
			}
			cached = r.accessToken;
			return r.accessToken;
		})
		.finally(() => {
			if (inflight === request) inflight = null;
		});
	inflight = request;
	return request;
}

/** Warm the cache without awaiting — called on mic pointerdown. */
export function prefetchDeepgramKey(): void {
	void getDeepgramKey().catch(() => {
		/* the tap path surfaces the error */
	});
}

/** Drop the cache: the key changed in Settings, or Deepgram rejected it. */
export function invalidateDeepgramKey(): void {
	generation++;
	cached = null;
	inflight = null;
}

// deepgramKey — page-session cache for the Deepgram credential coord hands out.
// The grant is a constant (coord returns the stored key with expiresIn: 0), but
// re-fetching it per recording costs a WAN round-trip on the tap path, delaying
// the WS open and the first caption. Cached here, prefetched on mic pointerdown,
// dropped when the key changes in Settings or Deepgram rejects it.
// Callers: MobileVoiceInput.tsx, Settings/TranscriptionPane.tsx.

import { coordClient } from "../connect.ts";

let cached: string | null = null;
let inflight: Promise<string> | null = null;

/** The cached key, the in-flight fetch, or a new fetch. Rejections propagate
 *  and leave the cache empty, so the next call retries. */
export function getDeepgramKey(): Promise<string> {
	if (cached !== null) return Promise.resolve(cached);
	if (inflight) return inflight;
	inflight = coordClient
		.transcriptionGrantToken({})
		.then((r) => {
			cached = r.accessToken;
			return r.accessToken;
		})
		.finally(() => {
			inflight = null;
		});
	return inflight;
}

/** Warm the cache without awaiting — called on mic pointerdown. */
export function prefetchDeepgramKey(): void {
	void getDeepgramKey().catch(() => {
		/* the tap path surfaces the error */
	});
}

/** Drop the cache: the key changed in Settings, or Deepgram rejected it. */
export function invalidateDeepgramKey(): void {
	cached = null;
}

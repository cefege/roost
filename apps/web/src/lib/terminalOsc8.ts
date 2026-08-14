// Browser OSC 8 registry. Parsing lives in @roost/shared/terminal-osc8; Sync
// delivers only completed coordinator-derived text-to-URI mappings.

import { Osc8Tracker } from "@roost/shared/terminal-osc8";

export { Osc8Tracker };

export type Osc8MappingSubscriber = (text: string, uri: string) => void;

const trackersBySession = new Map<string, Osc8Tracker>();
const subscribersBySession = new Map<string, Set<Osc8MappingSubscriber>>();

export function osc8TrackerFor(sessionId: string): Osc8Tracker {
  let tracker = trackersBySession.get(sessionId);
  if (!tracker) {
    tracker = new Osc8Tracker((text, uri) => {
      const subscribers = subscribersBySession.get(sessionId);
      if (!subscribers) return;
      for (const subscriber of subscribers) subscriber(text, uri);
    });
    trackersBySession.set(sessionId, tracker);
  }
  return tracker;
}

/** Record a completed mapping received from the coordinator. */
export function recordOsc8Link(sessionId: string, text: string, uri: string): void {
  osc8TrackerFor(sessionId).record(text, uri);
}

/** Subscribe to mappings stored for one terminal session. */
export function subscribeOsc8Mappings(
  sessionId: string,
  subscriber: Osc8MappingSubscriber,
): () => void {
  let subscribers = subscribersBySession.get(sessionId);
  if (!subscribers) {
    subscribers = new Set();
    subscribersBySession.set(sessionId, subscribers);
  }
  subscribers.add(subscriber);
  return () => {
    const current = subscribersBySession.get(sessionId);
    if (!current) return;
    current.delete(subscriber);
    if (current.size === 0) subscribersBySession.delete(sessionId);
  };
}

export function pruneOsc8Tracker(sessionId: string): void {
  trackersBySession.delete(sessionId);
  subscribersBySession.delete(sessionId);
}

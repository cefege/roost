import { afterEach, describe, expect, test } from "bun:test";
import {
  osc8TrackerFor,
  pruneOsc8Tracker,
  recordOsc8Link,
  subscribeOsc8Mappings,
} from "../src/lib/terminalOsc8.ts";

const sessionIds = ["osc8-a", "osc8-b"];

afterEach(() => {
  for (const sessionId of sessionIds) pruneOsc8Tracker(sessionId);
});

describe("session OSC 8 tracker registry", () => {
  test("retains mappings received before a pane obtains its tracker", () => {
    recordOsc8Link("osc8-a", "before-visit", "https://example.test/a");

    expect(osc8TrackerFor("osc8-a").lookup("before-visit")).toBe("https://example.test/a");
  });

  test("notifies session subscribers after storing the sanitized mapping", () => {
    const observed: Array<[string, string, string | undefined]> = [];
    const unsubscribe = subscribeOsc8Mappings("osc8-a", (text, uri) => {
      observed.push([text, uri, osc8TrackerFor("osc8-a").lookup(text)]);
    });

    recordOsc8Link("osc8-a", "\u001b[31mStyled.txt\u001b[0m", "https://example.test/styled");

    expect(observed).toEqual([[
      "Styled.txt",
      "https://example.test/styled",
      "https://example.test/styled",
    ]]);
    unsubscribe();
  });

  test("isolates mappings and notifications by session ID", () => {
    const notified: string[] = [];
    const unsubscribe = subscribeOsc8Mappings("osc8-a", (text) => notified.push(text));

    recordOsc8Link("osc8-a", "same-label", "https://example.test/a");
    recordOsc8Link("osc8-b", "same-label", "https://example.test/b");

    expect(osc8TrackerFor("osc8-a").lookup("same-label")).toBe("https://example.test/a");
    expect(osc8TrackerFor("osc8-b").lookup("same-label")).toBe("https://example.test/b");
    expect(notified).toEqual(["same-label"]);
    unsubscribe();
  });

  test("unsubscribe and pruning stop notifications without affecting another session", () => {
    let notifications = 0;
    const unsubscribe = subscribeOsc8Mappings("osc8-a", () => { notifications += 1; });
    unsubscribe();
    recordOsc8Link("osc8-a", "a-link", "https://example.test/a");
    recordOsc8Link("osc8-b", "b-link", "https://example.test/b");
    pruneOsc8Tracker("osc8-a");
    recordOsc8Link("osc8-a", "new-a", "https://example.test/new-a");

    expect(notifications).toBe(0);
    expect(osc8TrackerFor("osc8-a").lookup("a-link")).toBeUndefined();
    expect(osc8TrackerFor("osc8-b").lookup("b-link")).toBe("https://example.test/b");
  });
});

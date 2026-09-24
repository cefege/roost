// Tests the browser-safe STUN configuration boundary used by coordinator and peers.
// Operator strings must normalize to one safe wire representation or reject before
// they can select a relay, carry credentials, or grow unbounded configuration state.

import { describe, expect, test } from "bun:test";
import {
  DEFAULT_TERMINAL_PEER_STUN_URLS,
  parseTerminalPeerStunUrls,
} from "../src/terminal-peer.ts";

describe("terminal peer STUN configuration", () => {
  test("uses the account-free default only when the setting is absent", () => {
    expect(parseTerminalPeerStunUrls(undefined)).toEqual([...DEFAULT_TERMINAL_PEER_STUN_URLS]);
    expect(parseTerminalPeerStunUrls("")).toEqual([]);
  });

  test("normalizes declared DNS, IPv4, and IPv6 endpoints before sharing them", () => {
    expect(parseTerminalPeerStunUrls(
      "STUN:Stun.One.Example:3478,stun:192.0.2.8:5349,stun:[2001:DB8::8]:3478",
    )).toEqual([
      "stun:stun.one.example:3478",
      "stun:192.0.2.8:5349",
      "stun:[2001:db8::8]:3478",
    ]);
  });

  test("rejects credentials, URL extensions, relay schemes, control characters, and excess entries", () => {
    for (const raw of [
      "turn:relay.example:3478",
      "turns:relay.example:5349",
      "stuns:stun.example:5349",
      "stun:operator@stun.example:3478",
      "stun:stun.example/path",
      "stun:stun.example?transport=udp",
      "stun:stun.example#fragment",
      "stun:stun.example\n",
      "stun:one.example,stun:two.example,stun:three.example,stun:four.example,stun:five.example",
    ]) {
      expect(() => parseTerminalPeerStunUrls(raw), raw).toThrow();
    }
  });

  test("rejects duplicates after normalization rather than allowing ambiguous discovery policy", () => {
    expect(() => parseTerminalPeerStunUrls(
      "STUN:Stun.Example:3478,stun:stun.example:3478",
    )).toThrow("1 to 4 distinct stun: UDP URLs");
  });
});

// Unauthenticated loopback sockets are finite and expire before they can hold
// worker descriptors or LocalTerminalSockets rows indefinitely.

import { afterEach, expect, test, vi } from "bun:test";
import {
  TERMINAL_PEER_HELLO_DEADLINE_MS,
  TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER,
} from "@roost/shared/terminal-peer";
import { LocalTerminalPreHelloOwner } from "../src/local-terminal-prehello.ts";

afterEach(() => vi.useRealTimers());

test("caps and expires loopback sockets awaiting Hello", () => {
  vi.useFakeTimers();
  const expired: string[] = [];
  const owner = new LocalTerminalPreHelloOwner((socketId) => { expired.push(socketId); });
  for (let index = 0; index < TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER; index += 1) {
    expect(owner.admit(`socket-${index}`)).toBe(true);
  }
  expect(owner.admit("socket-overflow")).toBe(false);
  owner.clear("socket-0");
  expect(owner.admit("socket-replacement")).toBe(true);
  vi.advanceTimersByTime(TERMINAL_PEER_HELLO_DEADLINE_MS);
  expect(expired).toHaveLength(TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER);
  owner.dispose();
});

test("replaying one grant replaces its socket and authenticated grants stay capped", () => {
  const owner = new LocalTerminalPreHelloOwner(() => undefined);
  expect(owner.admit("socket-a")).toBe(true);
  expect(owner.authenticate("grant-a", "socket-a")).toEqual({
    admitted: true,
    replacedSocketId: null,
  });
  expect(owner.admit("socket-b")).toBe(true);
  expect(owner.authenticate("grant-a", "socket-b")).toEqual({
    admitted: true,
    replacedSocketId: "socket-a",
  });
  owner.retire("socket-a");

  for (let index = 1; index < TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER; index += 1) {
    const socketId = `authenticated-${index}`;
    expect(owner.admit(socketId)).toBe(true);
    expect(owner.authenticate(`grant-${index}`, socketId).admitted).toBe(true);
  }
  expect(owner.admit("authenticated-overflow")).toBe(true);
  expect(owner.authenticate("grant-overflow", "authenticated-overflow").admitted).toBe(false);
  owner.retire("authenticated-overflow");
  owner.dispose();
});

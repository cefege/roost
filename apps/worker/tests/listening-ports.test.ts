// Unit-covers the lsof NAME parse in listening-ports.ts: only ports with a
// NON-loopback bind survive, because the folder chip opens them at
// http://<worker reachable_addr>:<port> and a 127.x / [::1] server never
// answers on the tailnet IP (the "chips that never work" bug). The lsof
// subprocess itself isn't driven here — that's the humanchrome real-flow
// verify; this pins the classification. Sample rows are verbatim lsof -nP
// output captured on macOS.

import { test, expect } from "bun:test";
import { parseReachableListenPorts } from "../src/listening-ports.ts";

const HEADER = "COMMAND   PID USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME";

test("all-interfaces IPv4 (*) is kept; loopback v4/v6 dropped", () => {
  // Verbatim capture: one python pid bound *:5599, 127.0.0.1:5598, [::1]:5597.
  const out = [
    HEADER,
    "Python  86069 mike    3u  IPv4 0xc1436adcd6afc2ba      0t0  TCP *:5599 (LISTEN)",
    "Python  86069 mike    4u  IPv4 0x8909521add541cec      0t0  TCP 127.0.0.1:5598 (LISTEN)",
    "Python  86069 mike    5u  IPv6 0x5f3a8d20e6cef821      0t0  TCP [::1]:5597 (LISTEN)",
  ].join("\n");
  expect(parseReachableListenPorts(out)).toEqual([5599]);
});

test("0.0.0.0, [::], LAN + tailnet IPs are all reachable → kept, ascending", () => {
  const out = [
    HEADER,
    "node   1  m  20u  IPv4 0x0  0t0  TCP 0.0.0.0:3000 (LISTEN)",
    "node   1  m  21u  IPv6 0x0  0t0  TCP [::]:8080 (LISTEN)",
    "node   1  m  22u  IPv4 0x0  0t0  TCP 192.168.1.5:4000 (LISTEN)",
    "node   1  m  23u  IPv4 0x0  0t0  TCP 100.64.1.2:5173 (LISTEN)",
  ].join("\n");
  expect(parseReachableListenPorts(out)).toEqual([3000, 4000, 5173, 8080]);
});

test("loopback-only server (vite default) yields no chip", () => {
  const out = [
    HEADER,
    "node  9  m  20u  IPv4 0x0  0t0  TCP 127.0.0.1:5173 (LISTEN)",
    "node  9  m  21u  IPv6 0x0  0t0  TCP [::1]:5173 (LISTEN)",
    "node  9  m  22u  IPv4 0x0  0t0  TCP 127.0.0.1:9229 (LISTEN)", // node debugger
  ].join("\n");
  expect(parseReachableListenPorts(out)).toEqual([]);
});

test("a port bound both * and 127.0.0.1 is kept once (the * bind wins)", () => {
  const out = [
    HEADER,
    "srv  7  m  3u  IPv4 0x0  0t0  TCP *:5173 (LISTEN)",
    "srv  7  m  4u  IPv4 0x0  0t0  TCP 127.0.0.1:5173 (LISTEN)",
  ].join("\n");
  expect(parseReachableListenPorts(out)).toEqual([5173]);
});

test("empty / header-only / non-LISTEN noise → []", () => {
  expect(parseReachableListenPorts("")).toEqual([]);
  expect(parseReachableListenPorts(HEADER)).toEqual([]);
  // ESTABLISHED rows and prose never match the (LISTEN)-anchored regex.
  expect(parseReachableListenPorts(
    "node 1 m 5u IPv4 0x0 0t0 TCP 100.64.1.2:5173->100.64.1.9:52233 (ESTABLISHED)",
  )).toEqual([]);
});

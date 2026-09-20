// Tests bounded SDP admission before native or browser ICE processing begins.
// The accepted fixture is data-channel-only and uses documentation/mDNS candidates;
// failures assert fixed codes without retaining a credential, address, or SDP snapshot.

import { describe, expect, test } from "bun:test";
import {
  TERMINAL_PEER_MAX_MESSAGE_SIZE,
  TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE,
  TERMINAL_PEER_SDP_MAX_CANDIDATES,
  TERMINAL_PEER_SDP_MAX_LINES,
  TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES,
  TERMINAL_PEER_SDP_MAX_UTF8_BYTES,
} from "../src/terminal-peer.ts";
import {
  filterBrowserTerminalPeerUdpCandidates,
  inspectTerminalPeerSdp,
  normalizeTerminalPeerSha256Fingerprint,
  TerminalPeerSdpError,
} from "../src/terminal-peer-sdp.ts";

const FINGERPRINT = Array.from({ length: 32 }, (_, index) => index.toString(16).padStart(2, "0"))
  .join(":");
const OTHER_FINGERPRINT = Array.from({ length: 32 }, (_, index) => (255 - index).toString(16).padStart(2, "0"))
  .join(":");
const ICE_PASSWORD = "p".repeat(22);
const HOST_CANDIDATE = "a=candidate:host 1 udp 2122260223 192.0.2.8 5000 typ host";
const MDNS_CANDIDATE = "a=candidate:mdns 1 udp 2122260222 browser-a.local 5001 typ host";
const SRFLX_CANDIDATE = "a=candidate:srflx 1 udp 2122260221 198.51.100.8 5002 typ srflx raddr 0.0.0.0 rport 9";

function validSdp(extraApplicationLines: readonly string[] = []): string {
  return [
    "v=0",
    "o=- 1 2 IN IP4 127.0.0.1",
    "s=-",
    "t=0 0",
    "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
    "a=setup:actpass",
    `a=fingerprint:sha-256 ${FINGERPRINT}`,
    "a=ice-ufrag:offer-ufrag",
    `a=ice-pwd:${ICE_PASSWORD}`,
    `a=max-message-size:${TERMINAL_PEER_MAX_MESSAGE_SIZE}`,
    HOST_CANDIDATE,
    MDNS_CANDIDATE,
    SRFLX_CANDIDATE,
    ...extraApplicationLines,
    "",
  ].join("\r\n");
}

function sdpCode(action: () => unknown): string {
  try {
    action();
  } catch (error) {
    expect(error).toBeInstanceOf(TerminalPeerSdpError);
    return (error as TerminalPeerSdpError).code;
  }
  throw new Error("expected SDP rejection");
}

describe("terminal peer SDP admission", () => {
  test("accepts exactly the bounded UDP/DTLS/SCTP data-channel contract", () => {
    expect(inspectTerminalPeerSdp(validSdp())).toEqual({
      fingerprintSha256: FINGERPRINT.replaceAll(":", ""),
      candidateCount: 3,
      candidateTypes: ["host", "srflx"],
      maxMessageSize: TERMINAL_PEER_MAX_MESSAGE_SIZE,
    });
    expect(normalizeTerminalPeerSha256Fingerprint(FINGERPRINT.toUpperCase()))
      .toBe(FINGERPRINT.replaceAll(":", ""));
    expect(inspectTerminalPeerSdp(validSdp().replace(
      HOST_CANDIDATE,
      "a=candidate:loopback 1 udp 2122260223 127.0.0.1 5000 typ host",
    )).candidateTypes).toContain("host");
  });

  test("filters browser ICE-TCP candidates before UDP-only signaling", () => {
    const tcpCandidate = "a=candidate:tcp 1 tcp 2122260223 192.0.2.8 9 typ host tcptype passive";
    const filtered = filterBrowserTerminalPeerUdpCandidates(validSdp([tcpCandidate]));
    expect(filtered).not.toContain(tcpCandidate);
    expect(inspectTerminalPeerSdp(filtered).candidateCount).toBe(3);
  });

  test("rejects media sections and conflicting DTLS identities", () => {
    expect(sdpCode(() => inspectTerminalPeerSdp(validSdp([
      "m=audio 9 UDP/TLS/RTP/SAVPF 111",
    ])))).toBe("media");
    expect(sdpCode(() => inspectTerminalPeerSdp(validSdp([
      `a=fingerprint:sha-256 ${OTHER_FINGERPRINT}`,
    ])))).toBe("fingerprint");
  });

  test("normalizes RFC message-size defaults while rejecting sizes below the packet contract", () => {
    expect(sdpCode(() => inspectTerminalPeerSdp(validSdp().replace(`a=ice-pwd:${ICE_PASSWORD}\r\n`, ""))))
      .toBe("ice-credentials");

    const implicitDefault = inspectTerminalPeerSdp(validSdp().replace(
      `a=max-message-size:${TERMINAL_PEER_MAX_MESSAGE_SIZE}\r\n`,
      "",
    ));
    expect(implicitDefault.maxMessageSize).toBe(TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE);

    const unbounded = inspectTerminalPeerSdp(validSdp().replace(
      `a=max-message-size:${TERMINAL_PEER_MAX_MESSAGE_SIZE}`,
      "a=max-message-size:0",
    ));
    expect(unbounded.maxMessageSize).toBeNull();

    expect(sdpCode(() => inspectTerminalPeerSdp(validSdp().replace(
      `a=max-message-size:${TERMINAL_PEER_MAX_MESSAGE_SIZE}`,
      `a=max-message-size:${TERMINAL_PEER_MAX_MESSAGE_SIZE - 1}`,
    )))).toBe("max-message-size");
  });

  test("rejects candidates that could change the authorized UDP reachability boundary", () => {
    const invalidCandidates = [
      "a=candidate:tcp 1 tcp 2122260223 192.0.2.8 5000 typ host",
      "a=candidate:component 2 udp 2122260223 192.0.2.8 5000 typ host",
      "a=candidate:relay 1 udp 2122260223 192.0.2.8 5000 typ relay",
      "a=candidate:hostname 1 udp 2122260223 arbitrary.example 5000 typ host",
      "a=candidate:unspecified 1 udp 2122260223 0.0.0.0 5000 typ host",
      "a=candidate:multicast 1 udp 2122260223 224.0.0.1 5000 typ host",
    ];
    for (const candidate of invalidCandidates) {
      expect(sdpCode(() => inspectTerminalPeerSdp(validSdp([candidate]))), candidate).toBe("candidate");
    }
  });

  test("rejects resource-bound SDP, line, and candidate overflows before negotiation", () => {
    expect(sdpCode(() => inspectTerminalPeerSdp("x".repeat(TERMINAL_PEER_SDP_MAX_UTF8_BYTES + 1))))
      .toBe("sdp-size");
    expect(sdpCode(() => inspectTerminalPeerSdp(`${"a=x".padEnd(TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES + 1, "x")}\r\n`)))
      .toBe("sdp-line");
    expect(sdpCode(() => inspectTerminalPeerSdp([
      ...Array.from({ length: TERMINAL_PEER_SDP_MAX_LINES + 1 }, () => "a=x"),
    ].join("\r\n")))).toBe("sdp-lines");
    expect(sdpCode(() => inspectTerminalPeerSdp(validSdp(
      Array.from({ length: TERMINAL_PEER_SDP_MAX_CANDIDATES - 2 }, () => HOST_CANDIDATE),
    )))).toBe("candidate-count");
  });
});

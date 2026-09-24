// Bounded SDP inspection for authenticated terminal-peer signaling.
// It permits only one WebRTC data-channel media section and returns metadata
// callers may fence on without returning ICE credentials or candidate addresses.

import {
  TERMINAL_PEER_MAX_MESSAGE_SIZE,
  TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE,
  TERMINAL_PEER_SDP_ICE_PASSWORD_MAX_UTF8_BYTES,
  TERMINAL_PEER_SDP_ICE_PASSWORD_MIN_UTF8_BYTES,
  TERMINAL_PEER_SDP_ICE_UFRAG_MAX_UTF8_BYTES,
  TERMINAL_PEER_SDP_MAX_CANDIDATES,
  TERMINAL_PEER_SDP_MAX_LINES,
  TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES,
  TERMINAL_PEER_SDP_MAX_UTF8_BYTES,
} from "./terminal-peer.ts";

export type TerminalPeerSdpErrorCode =
  | "sdp-size"
  | "sdp-lines"
  | "sdp-line"
  | "media"
  | "fingerprint"
  | "ice-credentials"
  | "max-message-size"
  | "candidate-count"
  | "candidate";

export class TerminalPeerSdpError extends Error {
  readonly code: TerminalPeerSdpErrorCode;

  constructor(code: TerminalPeerSdpErrorCode, detail?: string) {
    super(`terminal peer SDP rejected: ${code}${detail ? `:${detail}` : ""}`);
    this.name = "TerminalPeerSdpError";
    this.code = code;
  }
}
export type TerminalPeerCandidateType = "host" | "srflx" | "prflx";
const TERMINAL_PEER_CANDIDATE_TYPE_ORDER: readonly TerminalPeerCandidateType[] = ["host", "srflx", "prflx"];
export interface TerminalPeerSdpMetadata {
  /** Lowercase, colon-free SHA-256 DTLS fingerprint. */
  readonly fingerprintSha256: string;
  readonly candidateCount: number;
  readonly candidateTypes: readonly TerminalPeerCandidateType[];
  /** Null represents SDP's explicit unlimited max-message-size value. */
  readonly maxMessageSize: number | null;
}

interface SdpScope {
  fingerprintSha256: string | null;
  iceUfrag: string | null;
  icePassword: string | null;
  maxMessageSize: number | null;
}

/** Inspects an offer or answer; invalid input throws without including SDP in the error. */
export function inspectTerminalPeerSdp(sdp: string): TerminalPeerSdpMetadata {
  if (utf8LengthAtMost(sdp, TERMINAL_PEER_SDP_MAX_UTF8_BYTES) === null) {
    throw new TerminalPeerSdpError("sdp-size");
  }

  const lines = sdp.split(/\r\n|\r|\n/u);
  if (lines.at(-1) === "") lines.pop();
  if (lines.length === 0 || lines.length > TERMINAL_PEER_SDP_MAX_LINES) {
    throw new TerminalPeerSdpError("sdp-lines");
  }

  const session = emptyScope();
  let application: SdpScope | null = null;
  let scope = session;
  let candidateCount = 0;
  const candidateTypes: Partial<Record<TerminalPeerCandidateType, true>> = {};

  for (const line of lines) {
    if (!isValidSdpLine(line)) throw new TerminalPeerSdpError("sdp-line");
    if (line.startsWith("m=")) {
      if (application !== null || !isApplicationDataChannelLine(line)) {
        throw new TerminalPeerSdpError("media");
      }
      application = emptyScope();
      scope = application;
      continue;
    }
    if (!line.startsWith("a=")) continue;

    if (line.startsWith("a=fingerprint:")) {
      assignFingerprint(scope, line);
      continue;
    }
    if (line.startsWith("a=ice-ufrag:")) {
      assignIceUfrag(scope, line.slice("a=ice-ufrag:".length));
      continue;
    }
    if (line.startsWith("a=ice-pwd:")) {
      assignIcePassword(scope, line.slice("a=ice-pwd:".length));
      continue;
    }
    if (line.startsWith("a=max-message-size:")) {
      assignMaxMessageSize(scope, line.slice("a=max-message-size:".length));
      continue;
    }
    if (line.startsWith("a=candidate:")) {
      if (application === null) throw new TerminalPeerSdpError("candidate");
      if (candidateCount >= TERMINAL_PEER_SDP_MAX_CANDIDATES) {
        throw new TerminalPeerSdpError("candidate-count");
      }
      candidateTypes[inspectCandidate(line.slice("a=candidate:".length))] = true;
      candidateCount += 1;
      continue;
    }
    if (line.startsWith("a=remote-candidates:") || line.startsWith("a=crypto:")) {
      throw new TerminalPeerSdpError("candidate");
    }
    if (line.startsWith("a=setup:") && !isValidDtlsSetup(line.slice("a=setup:".length))) {
      throw new TerminalPeerSdpError("media");
    }
  }

  if (application === null) throw new TerminalPeerSdpError("media");
  if (
    session.fingerprintSha256 !== null
    && application.fingerprintSha256 !== null
    && session.fingerprintSha256 !== application.fingerprintSha256
  ) {
    throw new TerminalPeerSdpError("fingerprint");
  }
  const fingerprintSha256 = effectiveValue(
    session.fingerprintSha256,
    application.fingerprintSha256,
    "fingerprint",
  );
  effectiveValue(session.iceUfrag, application.iceUfrag, "ice-credentials");
  effectiveValue(session.icePassword, application.icePassword, "ice-credentials");
  const advertisedMaxMessageSize = application.maxMessageSize ?? session.maxMessageSize;
  const maxMessageSize = advertisedMaxMessageSize === null
    ? TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE
    : advertisedMaxMessageSize === 0 ? null : advertisedMaxMessageSize;
  if (maxMessageSize !== null && maxMessageSize < TERMINAL_PEER_MAX_MESSAGE_SIZE) {
    throw new TerminalPeerSdpError("max-message-size");
  }

  return {
    fingerprintSha256,
    candidateCount,
    candidateTypes: TERMINAL_PEER_CANDIDATE_TYPE_ORDER.filter((type) => candidateTypes[type] === true),
    maxMessageSize,
  };
}
/** Removes browser-generated ICE-TCP candidates before the UDP-only offer crosses trust boundaries. */
export function filterBrowserTerminalPeerUdpCandidates(sdp: string): string {
  const lines = sdp.split(/\r\n|\r|\n/u);
  if (lines.at(-1) === "") lines.pop();
  const filtered = lines.filter((line) => !line.startsWith("a=candidate:") || line.slice("a=candidate:".length).split(" ")[2]?.toLowerCase() !== "tcp");
  return `${filtered.join("\r\n")}\r\n`;
}

/** Normalizes an SDP SHA-256 fingerprint without retaining its source formatting. */
export function normalizeTerminalPeerSha256Fingerprint(value: string): string | null {
  if (!/^(?:[0-9a-f]{2}:){31}[0-9a-f]{2}$/iu.test(value) && !/^[0-9a-f]{64}$/iu.test(value)) return null;
  return value.replaceAll(":", "").toLowerCase();
}

function emptyScope(): SdpScope {
  return {
    fingerprintSha256: null,
    iceUfrag: null,
    icePassword: null,
    maxMessageSize: null,
  };
}

function isValidSdpLine(line: string): boolean {
  if (line.length === 0 || utf8LengthAtMost(line, TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES) === null) {
    return false;
  }
  if (!/^[a-z]=/u.test(line)) return false;
  for (let index = 0; index < line.length; index += 1) {
    const code = line.charCodeAt(index);
    if (code < 0x20 || code > 0x7e) return false;
  }
  return true;
}

function isApplicationDataChannelLine(line: string): boolean {
  const fields = line.slice(2).split(" ");
  return fields.length === 4
    && fields[0] === "application"
    && /^(?:[1-9]\d{0,4})$/u.test(fields[1]!) && Number(fields[1]!) <= 65_535
    && /^udp\/dtls\/sctp$/iu.test(fields[2]!)
    && fields[3] === "webrtc-datachannel";
}

function assignFingerprint(scope: SdpScope, line: string): void {
  const match = /^a=fingerprint:sha-256 ((?:[0-9a-f]{2}:){31}[0-9a-f]{2})$/iu.exec(line);
  const fingerprint = match ? normalizeTerminalPeerSha256Fingerprint(match[1]!) : null;
  if (!fingerprint) throw new TerminalPeerSdpError("fingerprint");
  if (scope.fingerprintSha256 !== null && scope.fingerprintSha256 !== fingerprint) {
    throw new TerminalPeerSdpError("fingerprint");
  }
  scope.fingerprintSha256 = fingerprint;
}

function assignIceUfrag(scope: SdpScope, value: string): void {
  if (!isIceValue(value, 1, TERMINAL_PEER_SDP_ICE_UFRAG_MAX_UTF8_BYTES)) {
    throw new TerminalPeerSdpError("ice-credentials");
  }
  if (scope.iceUfrag !== null && scope.iceUfrag !== value) {
    throw new TerminalPeerSdpError("ice-credentials");
  }
  scope.iceUfrag = value;
}

function assignIcePassword(scope: SdpScope, value: string): void {
  if (!isIceValue(
    value,
    TERMINAL_PEER_SDP_ICE_PASSWORD_MIN_UTF8_BYTES,
    TERMINAL_PEER_SDP_ICE_PASSWORD_MAX_UTF8_BYTES,
  )) {
    throw new TerminalPeerSdpError("ice-credentials");
  }
  if (scope.icePassword !== null && scope.icePassword !== value) {
    throw new TerminalPeerSdpError("ice-credentials");
  }
  scope.icePassword = value;
}

function assignMaxMessageSize(scope: SdpScope, value: string): void {
  if (!/^\d+$/u.test(value)) throw new TerminalPeerSdpError("max-message-size");
  const maxMessageSize = Number(value);
  if (!Number.isSafeInteger(maxMessageSize) || maxMessageSize < 0) {
    throw new TerminalPeerSdpError("max-message-size");
  }
  if (scope.maxMessageSize !== null && scope.maxMessageSize !== maxMessageSize) {
    throw new TerminalPeerSdpError("max-message-size");
  }
  scope.maxMessageSize = maxMessageSize;
}

function effectiveValue<T>(
  sessionValue: T | null,
  applicationValue: T | null,
  error: TerminalPeerSdpErrorCode,
): T {
  if (applicationValue !== null) return applicationValue;
  if (sessionValue !== null) return sessionValue;
  throw new TerminalPeerSdpError(error);
}

function isIceValue(value: string, minBytes: number, maxBytes: number): boolean {
  if (!/^[\x21-\x7e]+$/u.test(value)) return false;
  const byteLength = utf8LengthAtMost(value, maxBytes);
  return byteLength !== null && byteLength >= minBytes;
}

function isValidDtlsSetup(value: string): boolean {
  return value === "actpass" || value === "active" || value === "passive";
}

function inspectCandidate(value: string): TerminalPeerCandidateType {
  const fields = value.split(" ");
  if (fields.length < 8 || fields.some((field) => field.length === 0)) {
    throw new TerminalPeerSdpError("candidate", "shape");
  }
  if (!/^[!-~]+$/u.test(fields[0]!)) throw new TerminalPeerSdpError("candidate", "foundation");
  if (fields[1] !== "1") throw new TerminalPeerSdpError("candidate", "component");
  if (fields[2]!.toLowerCase() !== "udp") throw new TerminalPeerSdpError("candidate", "protocol");
  if (!isUint32(fields[3]!)) throw new TerminalPeerSdpError("candidate", "priority");
  if (!isCandidateAddress(fields[4]!)) throw new TerminalPeerSdpError("candidate", "address");
  if (!isPort(fields[5]!)) throw new TerminalPeerSdpError("candidate", "port");
  if (fields[6]!.toLowerCase() !== "typ") throw new TerminalPeerSdpError("candidate", "marker");

  const candidateType = fields[7]!.toLowerCase();
  if (candidateType !== "host" && candidateType !== "srflx" && candidateType !== "prflx") {
    throw new TerminalPeerSdpError("candidate", "type");
  }
  if ((fields.length - 8) % 2 !== 0) throw new TerminalPeerSdpError("candidate", "extensions");

  let relatedAddress: string | undefined;
  let relatedPort: string | undefined;
  for (let index = 8; index < fields.length; index += 2) {
    const extension = fields[index]!.toLowerCase();
    const extensionValue = fields[index + 1]!;
    if (extension === "tcptype") throw new TerminalPeerSdpError("candidate", "tcp");
    if (extension === "raddr" && relatedAddress !== undefined) throw new TerminalPeerSdpError("candidate");
    if (extension === "raddr") relatedAddress = extensionValue;
    if (extension === "rport" && relatedPort !== undefined) throw new TerminalPeerSdpError("candidate");
    if (extension === "rport") relatedPort = extensionValue;
  }
  const redactedRelatedAddress = (relatedAddress === "0.0.0.0" || relatedAddress === "::")
    && (relatedPort === "0" || relatedPort === "9");
  if (
    (relatedAddress !== undefined || relatedPort !== undefined)
    && !redactedRelatedAddress
    && (!relatedAddress || !relatedPort || !isCandidateAddress(relatedAddress) || !isPort(relatedPort))
  ) throw new TerminalPeerSdpError("candidate", "related");
  return candidateType;
}

function isUint32(value: string): boolean {
  if (!/^(?:0|[1-9]\d{0,9})$/u.test(value)) return false;
  return Number(value) <= 0xffff_ffff;
}

function isPort(value: string): boolean {
  if (!/^(?:0|[1-9]\d{0,4})$/u.test(value)) return false;
  const port = Number(value);
  return port >= 1 && port <= 65_535;
}

function isCandidateAddress(value: string): boolean {
  if (isMdnsHostname(value)) return true;
  const ipv4 = parseIpv4(value);
  if (ipv4) return isIpv4Unicast(ipv4);
  const ipv6 = parseIpv6(value);
  if (!ipv6 || (ipv6[0]! & 0xff00) === 0xff00) return false;
  if (ipv6.every((word) => word === 0)) return false;
  if (
    ipv6[0] === 0 && ipv6[1] === 0 && ipv6[2] === 0 && ipv6[3] === 0
    && ipv6[4] === 0 && ipv6[5] === 0xffff
  ) {
    return isIpv4Unicast([
      ipv6[6]! >> 8,
      ipv6[6]! & 0xff,
      ipv6[7]! >> 8,
      ipv6[7]! & 0xff,
    ]);
  }
  return true;
}

function isMdnsHostname(value: string): boolean {
  if (!value.toLowerCase().endsWith(".local") || value.length <= ".local".length) return false;
  return value.split(".").every((label) =>
    /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/iu.test(label)
  );
}

function parseIpv4(value: string): number[] | null {
  const octets = value.split(".");
  if (octets.length !== 4) return null;
  const parsed: number[] = [];
  for (const octet of octets) {
    if (!/^(?:0|[1-9]\d{0,2})$/u.test(octet)) return null;
    const number = Number(octet);
    if (number > 255) return null;
    parsed.push(number);
  }
  return parsed;
}
function isIpv4Unicast(octets: readonly number[]): boolean {
  return octets[0]! !== 0 && octets[0]! < 224;
}

function parseIpv6(value: string): number[] | null {
  if (!value || value.includes("%") || value.includes("[") || value.includes("]")) return null;
  const halves = value.split("::");
  if (halves.length > 2) return null;
  const rawWords = [
    ...(halves[0] ? halves[0]!.split(":") : []),
    ...(halves.length === 2 && halves[1] ? halves[1]!.split(":") : []),
  ];
  const words: number[] = [];
  for (let index = 0; index < rawWords.length; index += 1) {
    const rawWord = rawWords[index]!;
    if (rawWord.includes(".")) {
      if (index !== rawWords.length - 1 || !value.endsWith(rawWord)) return null;
      const ipv4 = parseIpv4(rawWord);
      if (!ipv4) return null;
      words.push((ipv4[0]! << 8) | ipv4[1]!, (ipv4[2]! << 8) | ipv4[3]!);
      continue;
    }
    if (!/^[0-9a-f]{1,4}$/iu.test(rawWord)) return null;
    words.push(Number.parseInt(rawWord, 16));
  }
  if (halves.length === 1) return words.length === 8 ? words : null;
  if (words.length >= 8) return null;
  return [
    ...words.slice(0, halves[0] ? halves[0]!.split(":").length : 0),
    ...Array<number>(8 - words.length).fill(0),
    ...words.slice(halves[0] ? halves[0]!.split(":").length : 0),
  ];
}

function utf8LengthAtMost(value: string, maximum: number): number | null {
  let bytes = 0;
  for (let index = 0; index < value.length; index += 1) {
    const first = value.charCodeAt(index);
    if (first <= 0x7f) bytes += 1;
    else if (first <= 0x7ff) bytes += 2;
    else if (first >= 0xd800 && first <= 0xdbff && index + 1 < value.length) {
      const second = value.charCodeAt(index + 1);
      if (second >= 0xdc00 && second <= 0xdfff) {
        bytes += 4;
        index += 1;
      } else bytes += 3;
    } else bytes += 3;
    if (bytes > maximum) return null;
  }
  return bytes;
}

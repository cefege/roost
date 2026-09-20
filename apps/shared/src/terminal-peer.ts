// Shared browser-safe limits and channel definitions for authenticated terminal peers.
// Coordinator, worker, and browser import this contract rather than restating
// transport limits, data-channel identities, or STUN configuration parsing.

export const TERMINAL_PEER_WEBRTC_CAPABILITY = "terminal-peer-webrtc-v1";
export const TERMINAL_INPUT_ROUTE_CAPABILITY = "terminal-input-route-v1";

export const TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER = 32;
export const TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER = 4;
export const TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT = 8;
export const TERMINAL_PEER_MAX_SESSIONS_PER_GRANT = 256;
export const TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS = 64;
export const TERMINAL_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE = 8;

export const TERMINAL_PEER_NEGOTIATION_DEADLINE_MS = 15_000;
export const TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS = 3_000;
export const TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS = 8_000;
export const TERMINAL_PEER_PACKET_STALL_MS = 10_000;
export const TERMINAL_PEER_HELLO_DEADLINE_MS = 3_000;
export const TERMINAL_PEER_HEARTBEAT_INTERVAL_MS = 5_000;
export const TERMINAL_PEER_PROBE_DEADLINE_MS = 3_000;
export const TERMINAL_PEER_PROBE_QUALIFICATION_MS =
  TERMINAL_PEER_HEARTBEAT_INTERVAL_MS + TERMINAL_PEER_PROBE_DEADLINE_MS;

export const TERMINAL_PEER_SDP_MAX_UTF8_BYTES = 64 * 1024;
export const TERMINAL_PEER_SDP_MAX_LINES = 512;
export const TERMINAL_PEER_SDP_MAX_LINE_UTF8_BYTES = 4 * 1024;
export const TERMINAL_PEER_SDP_MAX_CANDIDATES = 64;
export const TERMINAL_PEER_SDP_ICE_UFRAG_MAX_UTF8_BYTES = 256;
export const TERMINAL_PEER_SDP_ICE_PASSWORD_MIN_UTF8_BYTES = 22;
export const TERMINAL_PEER_SDP_ICE_PASSWORD_MAX_UTF8_BYTES = 256;

export const TERMINAL_PEER_MAX_MESSAGE_SIZE = 16_384;
export const TERMINAL_PEER_SDP_DEFAULT_MAX_MESSAGE_SIZE = 64 * 1024;
export const TERMINAL_PEER_PACKET_MAGIC = 0x3150_5452;
export const TERMINAL_PEER_PACKET_HEADER_BYTES = 16;
export const TERMINAL_PEER_PACKET_MAX_BYTES = TERMINAL_PEER_MAX_MESSAGE_SIZE;
export const TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES = TERMINAL_PEER_PACKET_MAX_BYTES
  - TERMINAL_PEER_PACKET_HEADER_BYTES;

export type TerminalPeerPacketLane = "control" | "terminal" | "history";

export const TERMINAL_PEER_LANE_PRIORITY: readonly TerminalPeerPacketLane[] = [
  "control",
  "terminal",
  "history",
];

export const TERMINAL_PEER_LOGICAL_FRAME_MAX_BYTES: Readonly<Record<TerminalPeerPacketLane, number>> = {
  control: 128 * 1024,
  terminal: 2 * 1024 * 1024,
  history: 64 * 1024 * 1024,
};

export const TERMINAL_PEER_UNAUTHENTICATED_CONTROL_MAX_BYTES = 4 * 1024;
export const TERMINAL_PEER_CONTROL_QUEUE_MAX_BYTES = 256 * 1024;
export const TERMINAL_PEER_APPLICATION_QUEUE_MAX_BYTES = 64 * 1024 * 1024;
export const TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES = 128 * 1024 * 1024;
export const TERMINAL_PEER_WORKER_CONTROL_QUEUE_MAX_BYTES = 32 * 256 * 1024;
export const TERMINAL_PEER_WORKER_QUEUE_MAX_BYTES = 136 * 1024 * 1024;
export const TERMINAL_PEER_MAX_FLUSH_BYTES_PER_TURN = 64 * 1024;
export const TERMINAL_PEER_MAX_HISTORY_READS_PER_PEER = 1;

export const TERMINAL_PEER_SCTP_SEND_BUFFER_BYTES = 256 * 1024;
export const TERMINAL_PEER_SCTP_RECEIVE_BUFFER_BYTES = 512 * 1024;
export const TERMINAL_PEER_SCTP_MAX_CHUNKS_ON_QUEUE = 2_048;

export interface TerminalPeerChannelWatermarks {
  readonly highBytes: number;
  readonly lowBytes: number;
}

export const TERMINAL_PEER_CHANNEL_WATERMARKS: Readonly<
  Record<TerminalPeerPacketLane, TerminalPeerChannelWatermarks>
> = {
  control: { highBytes: 32 * 1024, lowBytes: 8 * 1024 },
  terminal: { highBytes: 64 * 1024, lowBytes: 16 * 1024 },
  history: { highBytes: 16 * 1024, lowBytes: 4 * 1024 },
};

export const TERMINAL_PEER_DATA_CHANNEL_PROTOCOL = "roost.local-terminal.v1";

export interface TerminalPeerDataChannelDefinition {
  readonly lane: TerminalPeerPacketLane;
  readonly id: 0 | 1 | 2;
  readonly label: string;
  readonly protocol: typeof TERMINAL_PEER_DATA_CHANNEL_PROTOCOL;
}

export const TERMINAL_PEER_DATA_CHANNELS: readonly TerminalPeerDataChannelDefinition[] = [
  {
    lane: "control",
    id: 0,
    label: "roost-terminal-control-v1",
    protocol: TERMINAL_PEER_DATA_CHANNEL_PROTOCOL,
  },
  {
    lane: "terminal",
    id: 1,
    label: "roost-terminal-data-v1",
    protocol: TERMINAL_PEER_DATA_CHANNEL_PROTOCOL,
  },
  {
    lane: "history",
    id: 2,
    label: "roost-terminal-history-v1",
    protocol: TERMINAL_PEER_DATA_CHANNEL_PROTOCOL,
  },
];

export const TERMINAL_PEER_INPUT_WORK_MAX_OUTSTANDING = 256;
export const TERMINAL_PEER_INPUT_WORK_MAX_RETAINED_BYTES = 16 * 1024 * 1024;
export const TERMINAL_PEER_INPUT_WORK_MAX_OUTSTANDING_PER_PORT = 32;
export const TERMINAL_PEER_INPUT_WORK_MAX_RETAINED_BYTES_PER_PORT = 2 * 1024 * 1024;
export const TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING = 32;
export const TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING_PER_PORT = 4;
export const TERMINAL_PEER_ROUTE_CLAIM_MAX_OUTSTANDING_PER_ACTOR_SESSION = 1;

export const DEFAULT_TERMINAL_PEER_STUN_URLS = ["stun:stun.cloudflare.com:3478"] as const;
export const TERMINAL_PEER_STUN_URL_MAX_COUNT = 4;

const STUN_URL_LIST_ERROR = "ROOST_TERMINAL_PEER_STUN_URLS must contain 1 to 4 distinct stun: UDP URLs";
const STUN_URL_ERROR = "ROOST_TERMINAL_PEER_STUN_URLS contains an invalid STUN URL";

/** Parses only operator-declared, non-relay STUN URLs suitable for both peers. */
export function parseTerminalPeerStunUrls(raw: string | undefined): string[] {
  if (raw === undefined) return [...DEFAULT_TERMINAL_PEER_STUN_URLS];
  if (raw === "") return [];
  if (hasControlCharacter(raw)) throw new Error(STUN_URL_ERROR);

  const urls = raw.split(",");
  if (urls.length === 0 || urls.length > TERMINAL_PEER_STUN_URL_MAX_COUNT) {
    throw new Error(STUN_URL_LIST_ERROR);
  }

  const normalized = urls.map(normalizeTerminalPeerStunUrl);
  if (new Set(normalized).size !== normalized.length) {
    throw new Error(STUN_URL_LIST_ERROR);
  }
  return normalized;
}

function normalizeTerminalPeerStunUrl(value: string): string {
  if (!value || value !== value.trim() || /[/?#@\\]/u.test(value)) {
    throw new Error(STUN_URL_ERROR);
  }
  const scheme = /^stun:(.+)$/iu.exec(value);
  if (!scheme) throw new Error(STUN_URL_ERROR);

  const authority = scheme[1]!;
  const bracketed = /^\[([^\]]+)\](?::(\d+))?$/u.exec(authority);
  if (bracketed) {
    if (!isIpv6Address(bracketed[1]!)) throw new Error(STUN_URL_ERROR);
    return `stun:[${bracketed[1]!.toLowerCase()}]${normalizePort(bracketed[2])}`;
  }

  if (authority.includes(":")) {
    const colon = authority.lastIndexOf(":");
    const host = authority.slice(0, colon);
    const port = authority.slice(colon + 1);
    if (!host || !port || (!isIpv4Address(host) && !isDnsName(host))) throw new Error(STUN_URL_ERROR);
    return `stun:${host.toLowerCase()}${normalizePort(port)}`;
  }
  if (!isIpv4Address(authority) && !isDnsName(authority)) throw new Error(STUN_URL_ERROR);
  return `stun:${authority.toLowerCase()}`;
}

function normalizePort(raw: string | undefined): string {
  if (raw === undefined) return "";
  if (!/^(?:0|[1-9]\d{0,4})$/u.test(raw)) throw new Error(STUN_URL_ERROR);
  const port = Number(raw);
  if (port < 1 || port > 65_535) throw new Error(STUN_URL_ERROR);
  return `:${port}`;
}


function isDnsName(value: string): boolean {
  if (value.length === 0 || value.length > 253 || /^\d+(?:\.\d+)*$/u.test(value)) return false;
  return value.split(".").every((label) =>
    /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/iu.test(label)
  );
}

function isIpv4Address(value: string): boolean {
  const octets = value.split(".");
  return octets.length === 4 && octets.every((octet) => {
    if (!/^(?:0|[1-9]\d{0,2})$/u.test(octet)) return false;
    return Number(octet) <= 255;
  });
}

function isIpv6Address(value: string): boolean {
  if (!value || value.includes("%") || value.includes("[") || value.includes("]")) return false;
  const doubleColon = value.indexOf("::");
  if (doubleColon !== value.lastIndexOf("::")) return false;
  const parts = value.split("::");
  if (parts.length > 2) return false;
  const units = parts.flatMap((part) => part ? part.split(":") : []);
  let unitCount = 0;
  for (let index = 0; index < units.length; index += 1) {
    const unit = units[index]!;
    if (unit.includes(".")) {
      if (index !== units.length - 1 || !value.endsWith(unit) || !isIpv4Address(unit)) return false;
      unitCount += 2;
      continue;
    }
    if (!/^[0-9a-f]{1,4}$/iu.test(unit)) return false;
    unitCount += 1;
  }
  return doubleColon === -1 ? unitCount === 8 : unitCount < 8;
}

function hasControlCharacter(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

// Malformed authenticated control packets used only by the source smoke adapter.
// TerminalPeerPacketPort injects one at its real reassembly boundary so parser
// rejection closes that peer generation without touching PTYs or other ports.
// No browser, environment variable, or production command can construct this fault.

import { TERMINAL_PEER_PACKET_MAGIC } from "@roost/protocol/terminal-peer-packets";

export type TerminalPeerMalformedPacketKind = "offset" | "total" | "id";

export function malformedTerminalPeerControlPacket(kind: TerminalPeerMalformedPacketKind): Uint8Array {
  const packet = new Uint8Array(17);
  const view = new DataView(packet.buffer, packet.byteOffset, 16);
  view.setUint32(0, TERMINAL_PEER_PACKET_MAGIC, true);
  view.setUint32(4, kind === "id" ? 0 : 1, true);
  view.setUint32(8, kind === "total" ? 0 : 1, true);
  view.setUint32(12, kind === "offset" ? 1 : 0, true);
  packet[16] = 1;
  return packet;
}

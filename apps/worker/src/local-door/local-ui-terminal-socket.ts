// Loopback terminal port adapter for Bun's local UI WebSocket.
// LocalUiServer owns routing and lifecycle while LocalTerminalSockets owns frame
// admission; this adapter only maps native send ownership to the common port.

import type { TerminalPacketPort, TerminalPacketSendResult } from "../terminal/peer/terminal-packet-port.ts";

export interface LocalTerminalSocket extends TerminalPacketPort {
  readonly kind: "loopback";
}

export interface LocalTerminalSocketHandlers {
  onOpen(port: TerminalPacketPort): void;
  onMessage(port: TerminalPacketPort, data: Uint8Array): void;
  onClose(port: TerminalPacketPort): void;
}

export interface BunLoopbackSocket {
  send(bytes: Uint8Array): number;
  close(code?: number, reason?: string): void;
  readonly open: boolean;
}

export class LoopbackTerminalPacketPort implements LocalTerminalSocket {
  readonly kind = "loopback" as const;
  private backpressuredBytes = 0;

  constructor(
    readonly socketId: string,
    private readonly socket: BunLoopbackSocket,
    private readonly maxBackpressureBytes: number,
  ) {}

  get open(): boolean {
    return this.socket.open;
  }

  bufferedBytes(): number {
    return this.backpressuredBytes;
  }

  send(
    bytes: Uint8Array,
    _lane: "control" | "terminal" | "history",
  ): TerminalPacketSendResult {
    if (!this.socket.open) return "refused";
    const result = this.socket.send(bytes);
    if (result > 0) {
      this.backpressuredBytes = 0;
      return "accepted";
    }
    if (result < 0) {
      this.backpressuredBytes += bytes.byteLength;
      return this.backpressuredBytes <= this.maxBackpressureBytes ? "backpressured" : "refused";
    }
    return "refused";
  }

  close(code?: number, reason?: string): void {
    this.socket.close(code, reason);
  }
}

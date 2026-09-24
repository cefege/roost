// Production packet-codec exercise for the Chromium/native runtime qualification.
// qualify-runtime owns the generation lifecycle and passes raw DataChannel packets here.
// This module uses the shared queue and assembler without another framing scheme.

import {
  TerminalPeerPacketAssembler,
  TerminalPeerPacketQueue,
  type TerminalPeerPacketQuota,
} from "@roost/protocol/terminal-peer-packets";
import { TERMINAL_PEER_LANE_PRIORITY, type TerminalPeerPacketLane } from "@roost/protocol/terminal-peer";
import {
  asQualificationFailure,
  checksumHex,
  createDeferred,
  qualificationFailure,
  type Deferred,
  type QualificationGeneration,
} from "./qualification-common.ts";

const PACKET_LANES = TERMINAL_PEER_LANE_PRIORITY;
const PACKET_MESSAGE_SIZES: Readonly<Record<TerminalPeerPacketLane, readonly number[]>> = {
  control: [64 * 1024 + 1, 257],
  terminal: [1024 * 1024 + 1, 313],
  history: [64 * 1024 + 3, 521],
};

type PacketMessages = Readonly<Record<TerminalPeerPacketLane, readonly Uint8Array[]>>;
type BrowserPacketSink = (lane: TerminalPeerPacketLane, packet: Uint8Array) => Promise<void>;
type NativePacketSink = (packet: Uint8Array) => void;

export interface NativePacketQualification {
  sendNativePackets(lane: TerminalPeerPacketLane, sendPacket: NativePacketSink): void;
  receiveBrowserPacket(lane: TerminalPeerPacketLane, packet: Uint8Array): void;
}

export interface ProductionPacketQualification extends NativePacketQualification {
  sendBrowserPackets(sendPacket: BrowserPacketSink): Promise<void>;
  receiveNativePacket(lane: TerminalPeerPacketLane, packet: Uint8Array): void;
  waitForTransfers(): Promise<void>;
  assertTransfersComplete(): void;
  dispose(): void;
}

export function createProductionPacketQualification(
  generation: QualificationGeneration,
): ProductionPacketQualification {
  return new RuntimePacketQualification(generation);
}

class RuntimePacketQualification implements ProductionPacketQualification {
  private readonly browserToNativeSender: QualificationPacketSender;
  private readonly browserToNativeReceiver: QualificationPacketReceiver;
  private readonly nativeToBrowserSender: QualificationPacketSender;
  private readonly nativeToBrowserReceiver: QualificationPacketReceiver;

  constructor(private readonly generation: QualificationGeneration) {
    const browserToNativeMessages = createPacketMessages(generation, 0x31);
    const nativeToBrowserMessages = createPacketMessages(generation, 0x71);
    this.browserToNativeSender = new QualificationPacketSender(
      generation,
      "packet-browser-send",
      browserToNativeMessages,
    );
    this.browserToNativeReceiver = new QualificationPacketReceiver(
      generation,
      "packet-browser-receive",
      browserToNativeMessages,
    );
    this.nativeToBrowserSender = new QualificationPacketSender(
      generation,
      "packet-native-send",
      nativeToBrowserMessages,
    );
    this.nativeToBrowserReceiver = new QualificationPacketReceiver(
      generation,
      "packet-native-receive",
      nativeToBrowserMessages,
    );
  }

  async sendBrowserPackets(sendPacket: BrowserPacketSink): Promise<void> {
    await this.browserToNativeSender.drainAll(sendPacket);
  }

  receiveNativePacket(lane: TerminalPeerPacketLane, packet: Uint8Array): void {
    this.nativeToBrowserReceiver.accept(lane, packet);
  }

  sendNativePackets(lane: TerminalPeerPacketLane, sendPacket: NativePacketSink): void {
    this.nativeToBrowserSender.drainLane(lane, sendPacket);
  }

  receiveBrowserPacket(lane: TerminalPeerPacketLane, packet: Uint8Array): void {
    this.browserToNativeReceiver.accept(lane, packet);
  }

  async waitForTransfers(): Promise<void> {
    await Promise.all([
      this.browserToNativeReceiver.waitForComplete(),
      this.nativeToBrowserReceiver.waitForComplete(),
    ]);
  }

  assertTransfersComplete(): void {
    this.browserToNativeReceiver.assertComplete();
    this.nativeToBrowserReceiver.assertComplete();
  }

  dispose(): void {
    this.browserToNativeSender.dispose();
    this.browserToNativeReceiver.dispose();
    this.nativeToBrowserSender.dispose();
    this.nativeToBrowserReceiver.dispose();
  }
}

class QualificationPacketSender {
  private readonly queues: Partial<Record<TerminalPeerPacketLane, TerminalPeerPacketQueue>> = {};
  private disposed = false;

  constructor(
    private readonly generation: QualificationGeneration,
    private readonly stage: string,
    messages: PacketMessages,
  ) {
    try {
      for (const lane of PACKET_LANES) {
        const queue = new TerminalPeerPacketQueue(lane, new QualificationPacketQuota(totalBytes(messages[lane])));
        this.queues[lane] = queue;
        for (const message of messages[lane]) {
          if (!queue.enqueue(message)) throw qualificationFailure(stage, generation, "packet_queue_refused");
        }
      }
    } catch (error) {
      this.dispose();
      throw asQualificationFailure(error, stage, generation, "packet_queue_create_failed");
    }
  }

  async drainAll(sendPacket: BrowserPacketSink): Promise<void> {
    this.assertOpen();
    for (const lane of PACKET_LANES) {
      const queue = this.queueFor(lane);
      try {
        for (let fragment = queue.nextFragment(); fragment; fragment = queue.nextFragment()) {
          await sendPacket(lane, fragment.bytes);
          fragment.commit();
        }
      } catch (error) {
        throw asQualificationFailure(error, this.stage, this.generation, "packet_send_failed");
      }
    }
  }

  drainLane(lane: TerminalPeerPacketLane, sendPacket: NativePacketSink): void {
    this.assertOpen();
    const queue = this.queueFor(lane);
    try {
      for (let fragment = queue.nextFragment(); fragment; fragment = queue.nextFragment()) {
        sendPacket(fragment.bytes);
        fragment.commit();
      }
    } catch (error) {
      throw asQualificationFailure(error, this.stage, this.generation, "packet_send_failed");
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const lane of PACKET_LANES) {
      const queue = this.queues[lane];
      if (!queue) continue;
      try {
        queue.clear();
      } catch {
        // Qualification teardown cannot recover a failed quota release.
      }
    }
  }

  private assertOpen(): void {
    if (this.disposed) throw qualificationFailure(this.stage, this.generation, "packet_sender_disposed");
  }

  private queueFor(lane: TerminalPeerPacketLane): TerminalPeerPacketQueue {
    const queue = this.queues[lane];
    if (!queue) throw qualificationFailure(this.stage, this.generation, "packet_lane_missing");
    return queue;
  }
}

class QualificationPacketReceiver {
  private readonly assemblers: Partial<Record<TerminalPeerPacketLane, TerminalPeerPacketAssembler>> = {};
  private readonly receivedCounts: Partial<Record<TerminalPeerPacketLane, number>> = {};
  private readonly completed: Deferred<void> = createDeferred<void>();
  private disposed = false;

  constructor(
    private readonly generation: QualificationGeneration,
    private readonly stage: string,
    private readonly messages: PacketMessages,
  ) {
    for (const lane of PACKET_LANES) {
      this.assemblers[lane] = new TerminalPeerPacketAssembler(lane, new QualificationPacketQuota(totalBytes(messages[lane])));
      this.receivedCounts[lane] = 0;
    }
  }

  accept(lane: TerminalPeerPacketLane, packet: Uint8Array): void {
    this.assertOpen();
    try {
      const receivedCount = this.receivedCounts[lane] ?? 0;
      if (receivedCount >= this.messages[lane].length) {
        throw qualificationFailure(this.stage, this.generation, "packet_unexpected_message");
      }
      const message = this.assemblerFor(lane).push(packet);
      if (!message) return;
      const expected = this.messages[lane][receivedCount];
      if (!expected || message.byteLength !== expected.byteLength || checksumHex(message) !== checksumHex(expected)) {
        throw qualificationFailure(this.stage, this.generation, "packet_checksum_or_order_mismatch");
      }
      this.receivedCounts[lane] = receivedCount + 1;
      if (this.isComplete()) this.completed.resolve();
    } catch (error) {
      const failure = asQualificationFailure(error, this.stage, this.generation, "packet_receive_failed");
      this.completed.reject(failure);
      throw failure;
    }
  }

  async waitForComplete(): Promise<void> {
    await this.completed.promise;
  }

  assertComplete(): void {
    if (!this.isComplete()) throw qualificationFailure(this.stage, this.generation, "packet_transfer_incomplete");
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const lane of PACKET_LANES) this.assemblerFor(lane).reset();
    if (!this.completed.settled()) {
      this.completed.reject(qualificationFailure(this.stage, this.generation, "packet_receiver_disposed"));
    }
  }

  private isComplete(): boolean {
    return PACKET_LANES.every((lane) =>
      this.receivedCounts[lane] === this.messages[lane].length && !this.assemblerFor(lane).hasPartialMessage
    );
  }

  private assertOpen(): void {
    if (this.disposed) throw qualificationFailure(this.stage, this.generation, "packet_receiver_disposed");
  }

  private assemblerFor(lane: TerminalPeerPacketLane): TerminalPeerPacketAssembler {
    const assembler = this.assemblers[lane];
    if (!assembler) throw qualificationFailure(this.stage, this.generation, "packet_lane_missing");
    return assembler;
  }
}

class QualificationPacketQuota implements TerminalPeerPacketQuota {
  private retainedBytes = 0;

  constructor(private readonly maximumBytes: number) {}

  reserve(bytes: number): boolean {
    if (!Number.isSafeInteger(bytes) || bytes < 0 || this.retainedBytes + bytes > this.maximumBytes) return false;
    this.retainedBytes += bytes;
    return true;
  }

  release(bytes: number): void {
    if (!Number.isSafeInteger(bytes) || bytes < 0 || bytes > this.retainedBytes) {
      throw new Error("qualification_packet_quota_release");
    }
    this.retainedBytes -= bytes;
  }
}

function createPacketMessages(generation: QualificationGeneration, directionSeed: number): PacketMessages {
  if (typeof generation !== "number") throw qualificationFailure("packet-create", generation, "packet_generation_required");
  return {
    control: PACKET_MESSAGE_SIZES.control.map((size, index) => createDeterministicBytes(size, generation, directionSeed, index)),
    terminal: PACKET_MESSAGE_SIZES.terminal.map((size, index) => createDeterministicBytes(size, generation, directionSeed, index)),
    history: PACKET_MESSAGE_SIZES.history.map((size, index) => createDeterministicBytes(size, generation, directionSeed, index)),
  };
}

function createDeterministicBytes(size: number, generation: number, directionSeed: number, messageIndex: number): Uint8Array {
  const bytes = new Uint8Array(size);
  let state = (Math.imul(generation, 0x9e37_79b1) + Math.imul(directionSeed, 0x85eb_ca6b) + messageIndex) >>> 0;
  for (let index = 0; index < bytes.length; index++) {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    bytes[index] = state >>> 24;
  }
  return bytes;
}

function totalBytes(messages: readonly Uint8Array[]): number {
  return messages.reduce((total, message) => total + message.byteLength, 0);
}

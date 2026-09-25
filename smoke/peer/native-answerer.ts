// node-datachannel answerer for finite Chromium runtime qualification.
// It owns three negotiated channels and sends shared-codec packets only in normal mode.
// Packet construction and verification stay in the driver-owned qualification helper.

import type { TerminalPeerNative } from "../../apps/worker/src/terminal/peer/terminal-peer-native.ts";
import type { TerminalPeerPacketLane } from "@roost/protocol/terminal-peer";
import {
  CLOSE_TIMEOUT_MS,
  CONNECTION_TIMEOUT_MS,
  NATIVE_UDP_PORT_RANGE_END,
  NATIVE_UDP_PORT_RANGE_START,
  OFFER_GATHERING_TIMEOUT_MS,
  QUALIFICATION_CHANNELS,
  QUALIFICATION_DATA_CHANNEL_PROTOCOL,
  asQualificationFailure,
  checksumHex,
  createDeferred,
  hasIceCandidate,
  normalizeSha256Fingerprint,
  qualificationFailure,
  waitForQualification,
  type Deferred,
  type QualificationGeneration,
  type QualificationMessages,
} from "./qualification-common.ts";
import type { NativePacketQualification } from "./packet-qualification.ts";

type NativeDescriptionType = "answer" | "offer" | "pranswer" | "rollback" | "unspec";

interface NativeDataChannel {
  sendMessageBinary(buffer: Buffer | Uint8Array): boolean;
  onOpen(callback: () => void): void;
  onClosed(callback: () => void): void;
  onError(callback: (error: string) => void): void;
  onMessage(callback: (message: string | Uint8Array | ArrayBuffer) => void): void;
}

interface NativeDataChannelConfig {
  protocol: string;
  negotiated: boolean;
  id: number;
  unordered: boolean;
}

interface NativePeerConnection {
  close(): void;
  setRemoteDescription(sdp: string, type: NativeDescriptionType): void;
  setLocalDescription(type?: NativeDescriptionType): void;
  localDescription(): { type: NativeDescriptionType; sdp: string } | null;
  gatheringState(): string;
  remoteFingerprint(): { algorithm: string; value: string };
  createDataChannel(label: string, config: NativeDataChannelConfig): NativeDataChannel;
  onLocalDescription(callback: (sdp: string, type: NativeDescriptionType) => void): void;
  onGatheringStateChange(callback: (state: string) => void): void;
  onStateChange(callback: (state: string) => void): void;
  onIceStateChange(callback: (state: string) => void): void;
  onDataChannel(callback: (channel: NativeDataChannel) => void): void;
}

interface NativeChannelState {
  readonly channel: NativeDataChannel;
  readonly lane: TerminalPeerPacketLane;
  opened: boolean;
  messagesSent: boolean;
  receivedMessages: number;
  closed: boolean;
}

export interface NativeAnswerer {
  acceptOffer(offerSdp: string): Promise<string>;
  waitForMessages(): Promise<void>;
  close(): Promise<void>;
  dispose(): void;
}

export function createNativeAnswerer(
  native: TerminalPeerNative,
  generation: QualificationGeneration,
  messages: QualificationMessages,
  expectedRemoteFingerprint: string,
  packetQualification?: NativePacketQualification,
): NativeAnswerer {
  try {
    return new NativeRuntimeAnswerer(
      native,
      generation,
      messages,
      expectedRemoteFingerprint,
      packetQualification,
    );
  } catch (error) {
    throw asQualificationFailure(error, "native-create", generation, "native_peer_create_failed");
  }
}

class NativeRuntimeAnswerer implements NativeAnswerer {
  private readonly peer: NativePeerConnection;
  private readonly gatheredAnswer: Deferred<string> = createDeferred<string>();
  private readonly receivedAllMessages: Deferred<void> = createDeferred<void>();
  private readonly allChannelsClosed: Deferred<void> = createDeferred<void>();
  private readonly nativeChannels: NativeChannelState[] = [];
  private acceptedOffer = false;
  private disposed = false;
  private nativeLocalDescriptionCallbackSeen = false;
  private nativeGatheringCallbackSeen = false;
  private nativeStateCallbackSeen = false;
  private nativeFingerprintSeen = false;
  private closedChannels = 0;

  constructor(
    native: TerminalPeerNative,
    private readonly generation: QualificationGeneration,
    private readonly messages: QualificationMessages,
    private readonly expectedRemoteFingerprint: string,
    private readonly packetQualification?: NativePacketQualification,
  ) {
    this.peer = new native.PeerConnection(`roost-peer-qualification-${generation}`, {
      iceServers: [],
      disableAutoNegotiation: true,
      enableIceTcp: false,
      disableFingerprintVerification: false,
      iceTransportPolicy: "all",
      maxMessageSize: 16_384,
      portRangeBegin: NATIVE_UDP_PORT_RANGE_START,
      portRangeEnd: NATIVE_UDP_PORT_RANGE_END,
    });
    this.installPeerCallbacks();
    this.createNegotiatedChannels();
  }

  async acceptOffer(offerSdp: string): Promise<string> {
    if (this.acceptedOffer) throw qualificationFailure("native-offer", this.generation, "duplicate_offer");
    this.acceptedOffer = true;
    try {
      this.peer.setRemoteDescription(offerSdp, "offer");
      this.peer.setLocalDescription("answer");
      this.resolveGatheredAnswer();
    } catch (error) {
      throw asQualificationFailure(error, "native-offer", this.generation, "native_offer_rejected");
    }
    return this.waitForGatheredAnswer();
  }

  async waitForMessages(): Promise<void> {
    await waitForQualification(
      "native-messages",
      this.generation,
      CONNECTION_TIMEOUT_MS,
      this.receivedAllMessages.promise,
    );
    if (
      this.nativeChannels.length !== QUALIFICATION_CHANNELS.length
      || !this.nativeFingerprintSeen
      || this.nativeChannels.some((state) => !state.opened || !state.messagesSent || state.receivedMessages !== this.messages.browserToNative.length)
    ) {
      throw qualificationFailure("native-messages", this.generation, "native_callbacks_incomplete");
    }
  }

  async close(): Promise<void> {
    if (this.nativeChannels.some((state) => !state.opened)) {
      throw qualificationFailure("native-close", this.generation, "native_channel_not_open");
    }
    try {
      this.peer.close();
    } catch (error) {
      throw asQualificationFailure(error, "native-close", this.generation, "native_peer_close_failed");
    }
    await waitForQualification(
      "native-close",
      this.generation,
      CLOSE_TIMEOUT_MS,
      this.allChannelsClosed.promise,
    );
    if (!this.nativeStateCallbackSeen || this.nativeChannels.some((state) => !state.closed)) {
      throw qualificationFailure("native-close", this.generation, "native_close_callbacks_incomplete");
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    try {
      this.peer.close();
    } catch {
      // A failed setup may already have released the native peer.
    }
  }

  private installPeerCallbacks(): void {
    this.peer.onLocalDescription((_descriptionSdp, type) => {
      if (type !== "answer") return;
      this.nativeLocalDescriptionCallbackSeen = true;
      this.resolveGatheredAnswer();
    });
    this.peer.onGatheringStateChange(() => {
      this.nativeGatheringCallbackSeen = true;
      this.resolveGatheredAnswer();
    });
    this.peer.onStateChange((state) => {
      this.nativeStateCallbackSeen = true;
      if (state === "failed") this.fail("native-state", "native_connection_failed");
    });
    this.peer.onIceStateChange((state) => {
      if (state === "failed") this.fail("native-ice", "native_ice_failed");
    });
    this.peer.onDataChannel(() => this.fail("native-datachannel", "unexpected_dcep_channel"));
  }

  private createNegotiatedChannels(): void {
    for (const channelDefinition of QUALIFICATION_CHANNELS) {
      const channel = this.peer.createDataChannel(channelDefinition.label, {
        negotiated: true,
        id: channelDefinition.id,
        unordered: false,
        protocol: QUALIFICATION_DATA_CHANNEL_PROTOCOL,
      });
      const state: NativeChannelState = {
        channel,
        lane: channelDefinition.lane,
        opened: false,
        messagesSent: false,
        receivedMessages: 0,
        closed: false,
      };
      this.installDataChannelCallbacks(state);
      this.nativeChannels.push(state);
    }
  }

  private installDataChannelCallbacks(state: NativeChannelState): void {
    state.channel.onOpen(() => {
      try {
        const fingerprint = this.peer.remoteFingerprint();
        if (
          fingerprint.algorithm !== "sha-256"
          || normalizeSha256Fingerprint(fingerprint.value) !== this.expectedRemoteFingerprint
        ) {
          this.fail("native-fingerprint", "native_remote_fingerprint_invalid");
          return;
        }
        this.nativeFingerprintSeen = true;
      } catch {
        this.fail("native-fingerprint", "native_remote_fingerprint_unavailable");
        return;
      }
      if (state.opened) {
        this.fail("native-datachannel", "duplicate_native_channel_open");
        return;
      }
      state.opened = true;
      try {
        for (const message of this.messages.nativeToBrowser) {
          // libdatachannel's false means buffered acceptance, never a refusal.
          state.channel.sendMessageBinary(Buffer.from(message.buffer, message.byteOffset, message.byteLength));
        }
        this.packetQualification?.sendNativePackets(state.lane, (packet) => {
          state.channel.sendMessageBinary(Buffer.from(packet.buffer, packet.byteOffset, packet.byteLength));
        });
        state.messagesSent = true;
      } catch {
        this.fail("native-send", "native_send_failed");
      }
    });
    state.channel.onMessage((message) => {
      if (typeof message === "string") {
        this.fail("native-messages", "native_non_binary_message");
        return;
      }
      const bytes = message instanceof ArrayBuffer
        ? new Uint8Array(message)
        : new Uint8Array(message.buffer, message.byteOffset, message.byteLength);
      const expectedMessage = this.messages.browserToNative[state.receivedMessages];
      if (expectedMessage) {
        if (bytes.byteLength !== expectedMessage.byteLength || checksumHex(bytes) !== checksumHex(expectedMessage)) {
          this.fail("native-messages", "native_message_checksum_mismatch");
          return;
        }
        state.receivedMessages++;
        if (this.nativeChannels.every((channelState) => channelState.receivedMessages === this.messages.browserToNative.length)) {
          this.receivedAllMessages.resolve();
        }
        return;
      }
      if (!this.packetQualification) {
        this.fail("native-messages", "native_message_order_mismatch");
        return;
      }
      try {
        this.packetQualification.receiveBrowserPacket(state.lane, bytes);
      } catch {
        this.fail("native-packets", "native_packet_receive_failed");
      }
    });
    state.channel.onError(() => this.fail("native-datachannel", "native_datachannel_error"));
    state.channel.onClosed(() => {
      if (state.closed) return;
      state.closed = true;
      this.closedChannels++;
      if (this.closedChannels === this.nativeChannels.length) this.allChannelsClosed.resolve();
      if (!this.receivedAllMessages.settled()) this.fail("native-close", "native_channel_closed_before_messages");
    });
  }

  private resolveGatheredAnswer(): void {
    if (this.gatheredAnswer.settled() || this.peer.gatheringState() !== "complete") return;
    const description = this.peer.localDescription();
    if (
      !description
      || description.type !== "answer"
      || !hasIceCandidate(description.sdp)
      || !this.nativeLocalDescriptionCallbackSeen
      || !this.nativeGatheringCallbackSeen
    ) return;
    this.gatheredAnswer.resolve(description.sdp);
  }

  private async waitForGatheredAnswer(): Promise<string> {
    const answerOrBoundedPartial = new Promise<string>((resolve, reject) => {
      const deadline = setTimeout(() => {
        const description = this.peer.localDescription();
        if (
          description
          && description.type === "answer"
          && hasIceCandidate(description.sdp)
          && this.nativeLocalDescriptionCallbackSeen
          && this.nativeGatheringCallbackSeen
        ) {
          resolve(description.sdp);
          return;
        }
        reject(qualificationFailure("native-gathering", this.generation, "native_answer_without_candidate"));
      }, OFFER_GATHERING_TIMEOUT_MS);
      this.gatheredAnswer.promise.then(
        (answerSdp) => {
          clearTimeout(deadline);
          resolve(answerSdp);
        },
        (error) => {
          clearTimeout(deadline);
          reject(error);
        },
      );
    });
    return await waitForQualification(
      "native-gathering",
      this.generation,
      OFFER_GATHERING_TIMEOUT_MS + 250,
      answerOrBoundedPartial,
    );
  }

  private fail(stage: string, code: string): void {
    const failure = qualificationFailure(stage, this.generation, code);
    this.gatheredAnswer.reject(failure);
    this.receivedAllMessages.reject(failure);
    this.allChannelsClosed.reject(failure);
  }
}

// Chromium-side offerer for finite native runtime qualification.
// It sends raw runtime-only messages and forwards production packet bytes without framing them.
// The Node driver owns packet queueing, reassembly, and payload verification.

import type { Page } from "@playwright/test";
import type { TerminalPeerPacketLane } from "@roost/shared/terminal-peer";
import {
  CLOSE_TIMEOUT_MS,
  CONNECTION_TIMEOUT_MS,
  OFFER_GATHERING_TIMEOUT_MS,
  QUALIFICATION_CHANNELS,
  QUALIFICATION_DATA_CHANNEL_PROTOCOL,
  asQualificationFailure,
  encodeBase64,
  qualificationFailure,
  runBrowserQualificationStep,
  sha256FingerprintFromSdp,
  waitForQualification,
  type QualificationGeneration,
  type QualificationMessages,
} from "./qualification-common.ts";

interface BrowserBootstrapInput {
  browserMessages: readonly string[];
  nativeSizes: readonly number[];
  channels: readonly { id: number; label: string }[];
  protocol: string;
  gatheringTimeoutMs: number;
  closeTimeoutMs: number;
  packetBinding: string | null;
}

export interface BrowserReceivedPayloads {
  readonly channels: readonly (readonly string[])[];
}

interface BrowserBootstrapResult {
  offerSdp: string;
}

interface BrowserQualificationState {
  acceptAnswer(answerSdp: string): Promise<void>;
  waitForMessages(): Promise<BrowserReceivedPayloads>;
  waitForChannels(): Promise<void>;
  sendPacket(channelId: number, encodedPacket: string): Promise<void>;
  close(): Promise<void>;
  dispose(): void;
}

declare global {
  interface Window {
    __roostPeerQualification?: BrowserQualificationState;
  }
}

type BrowserPacketReceiver = (lane: TerminalPeerPacketLane, packet: Uint8Array) => void;

export interface BrowserOfferer {
  readonly offerSdp: string;
  readonly offerFingerprint: string;
  acceptAnswer(answerSdp: string): Promise<void>;
  waitForMessages(): Promise<BrowserReceivedPayloads>;
  waitForChannels(): Promise<void>;
  sendPacket(lane: TerminalPeerPacketLane, packet: Uint8Array): Promise<void>;
  close(): Promise<void>;
  dispose(): Promise<void>;
}

export async function createBrowserOfferer(
  page: Page,
  generation: QualificationGeneration,
  messages: QualificationMessages,
  receivePacket?: BrowserPacketReceiver,
): Promise<BrowserOfferer> {
  const packetBinding = receivePacket ? "__roostPeerQualificationPacket" : null;
  if (receivePacket) {
    await page.exposeBinding(packetBinding!, async (_source, channelId: unknown, encodedPacket: unknown) => {
      const channel = typeof channelId === "number"
        ? QUALIFICATION_CHANNELS.find((candidate) => candidate.id === channelId)
        : undefined;
      if (!channel || typeof encodedPacket !== "string") {
        throw qualificationFailure("browser-packets", generation, "browser_packet_callback_invalid");
      }
      try {
        const packet = Buffer.from(encodedPacket, "base64");
        receivePacket(channel.lane, new Uint8Array(packet.buffer, packet.byteOffset, packet.byteLength));
      } catch (error) {
        throw asQualificationFailure(error, "browser-packets", generation, "browser_packet_callback_failed");
      }
    });
  }
  await runBrowserQualificationStep(page.setContent("<!doctype html><title>terminal peer qualification</title>"), "browser-page", generation);
  const bootstrap = await runBrowserQualificationStep(
    page.evaluate(async (input: BrowserBootstrapInput): Promise<BrowserBootstrapResult> => {
      type BrowserChannelState = {
        channel: RTCDataChannel;
        channelId: number;
        opened: boolean;
        messagesSent: boolean;
        receivedMessages: number;
        receivedPayloads: string[];
        closed: boolean;
        receiveChain: Promise<void>;
      };
      const decodeBase64 = (encoded: string): Uint8Array<ArrayBuffer> => {
        const text = atob(encoded);
        const bytes = new Uint8Array(new ArrayBuffer(text.length));
        for (let index = 0; index < text.length; index++) bytes[index] = text.charCodeAt(index);
        return bytes;
      };
      const encodeBase64 = (bytes: Uint8Array): string => {
        let text = "";
        for (const byte of bytes) text += String.fromCharCode(byte);
        return btoa(text);
      };
      const browserMessages = input.browserMessages.map(decodeBase64);
      const nativeSizes = [...input.nativeSizes];
      const packetBinding = input.packetBinding
        ? (globalThis as typeof globalThis & Record<string, (channelId: number, encodedPacket: string) => Promise<void>>)[input.packetBinding]
        : undefined;
      if (input.packetBinding && typeof packetBinding !== "function") throw new Error("browser_packet_binding_missing");
      let resolveCompletion!: () => void;
      let rejectCompletion!: (reason: Error) => void;
      let completionSettled = false;
      let verificationFailure: string | undefined;
      let resolveChannelsOpened!: () => void;
      let rejectChannelsOpened!: (reason: Error) => void;
      let channelsOpenedSettled = false;
      const completion = new Promise<void>((resolve, reject) => {
        resolveCompletion = resolve;
        rejectCompletion = reject;
      });
      const channelsOpened = new Promise<void>((resolve, reject) => {
        resolveChannelsOpened = resolve;
        rejectChannelsOpened = reject;
      });
      void completion.catch(() => undefined);
      void channelsOpened.catch(() => undefined);
      let resolveClosed!: () => void;
      const closed = new Promise<void>((resolve) => {
        resolveClosed = resolve;
      });
      let closedChannels = 0;
      const channelStates: BrowserChannelState[] = [];
      const reject = (code: string): void => {
        verificationFailure ??= code;
        if (!channelsOpenedSettled) {
          channelsOpenedSettled = true;
          rejectChannelsOpened(new Error(code));
        }
        if (completionSettled) return;
        completionSettled = true;
        rejectCompletion(new Error(code));
      };
      const peer = new RTCPeerConnection({ iceServers: [] });
      peer.ondatachannel = () => reject("browser_unexpected_dcep_channel");
      peer.onconnectionstatechange = () => {
        if (peer.connectionState === "failed") reject("browser_connection_failed");
      };
      peer.oniceconnectionstatechange = () => {
        if (peer.iceConnectionState === "failed") reject("browser_ice_failed");
      };
      for (const channelDefinition of input.channels) {
        const channel = peer.createDataChannel(channelDefinition.label, {
          negotiated: true,
          id: channelDefinition.id,
          ordered: true,
          protocol: input.protocol,
        });
        channel.binaryType = "arraybuffer";
        const channelState: BrowserChannelState = {
          channel,
          channelId: channelDefinition.id,
          opened: false,
          messagesSent: false,
          receivedMessages: 0,
          receivedPayloads: [],
          closed: false,
          receiveChain: Promise.resolve(),
        };
        channel.onopen = () => {
          if (channelState.opened) {
            reject("browser_duplicate_open");
            return;
          }
          channelState.opened = true;
          if (!channelsOpenedSettled && channelStates.every((state) => state.opened)) {
            channelsOpenedSettled = true;
            resolveChannelsOpened();
          }
          try {
            for (const message of browserMessages) channel.send(message);
            channelState.messagesSent = true;
          } catch {
            reject("browser_send_failed");
          }
        };
        channel.onmessage = (event) => {
          channelState.receiveChain = channelState.receiveChain.then(async () => {
            if (!(event.data instanceof ArrayBuffer)) {
              reject("browser_non_binary_message");
              return;
            }
            const expectedSize = nativeSizes[channelState.receivedMessages];
            if (expectedSize !== undefined) {
              const payload = new Uint8Array(event.data);
              if (payload.byteLength !== expectedSize) {
                reject("browser_message_size_mismatch");
                return;
              }
              channelState.receivedPayloads.push(encodeBase64(payload));
              channelState.receivedMessages++;
              if (channelStates.every((state) => state.receivedMessages === nativeSizes.length)) {
                completionSettled = true;
                resolveCompletion();
              }
              return;
            }
            if (!packetBinding) {
              reject("browser_message_order_mismatch");
              return;
            }
            await packetBinding(channelDefinition.id, encodeBase64(new Uint8Array(event.data)));
          }).catch(() => reject("browser_message_callback_failed"));
        };
        channel.onerror = () => reject("browser_datachannel_error");
        channel.onclose = () => {
          if (channelState.closed) return;
          channelState.closed = true;
          closedChannels++;
          if (closedChannels === channelStates.length) resolveClosed();
          if (!completionSettled) reject("browser_channel_closed_before_messages");
        };
        channelStates.push(channelState);
      }
      await peer.setLocalDescription(await peer.createOffer());
      const gatheringFinished = await Promise.race([
        new Promise<boolean>((resolve) => {
          if (peer.iceGatheringState === "complete") {
            resolve(true);
            return;
          }
          peer.addEventListener("icegatheringstatechange", () => {
            if (peer.iceGatheringState === "complete") resolve(true);
          }, { once: true });
        }),
        new Promise<boolean>((resolve) => window.setTimeout(() => resolve(false), input.gatheringTimeoutMs)),
      ]);
      const offerSdp = peer.localDescription?.sdp;
      if (!offerSdp || (!gatheringFinished && !/(?:^|\r?\n)a=candidate:/u.test(offerSdp))) {
        throw new Error("browser_offer_without_candidate");
      }
      window.__roostPeerQualification = {
        async acceptAnswer(answerSdp: string): Promise<void> {
          if (peer.signalingState === "closed") throw new Error("browser_peer_closed_before_answer");
          await peer.setRemoteDescription({ type: "answer", sdp: answerSdp });
        },
        async waitForMessages(): Promise<BrowserReceivedPayloads> {
          await completion;
          if (
            verificationFailure
            || channelStates.some((state) => !state.opened || !state.messagesSent || state.receivedMessages !== nativeSizes.length)
          ) {
            throw new Error(verificationFailure ?? "browser_callbacks_incomplete");
          }
          return { channels: channelStates.map((state) => state.receivedPayloads) };
        },
        async waitForChannels(): Promise<void> {
          await channelsOpened;
        },
        async sendPacket(channelId: number, encodedPacket: string): Promise<void> {
          await channelsOpened;
          const channelState = channelStates.find((state) => state.channelId === channelId);
          if (!channelState || !channelState.opened || channelState.channel.readyState !== "open") {
            throw new Error("browser_packet_channel_unavailable");
          }
          channelState.channel.send(decodeBase64(encodedPacket));
        },
        async close(): Promise<void> {
          peer.close();
          await Promise.race([
            closed,
            new Promise<void>((_, rejectClose) => window.setTimeout(() => rejectClose(new Error("browser_close_timeout")), input.closeTimeoutMs)),
          ]);
          await Promise.all(channelStates.map((state) => state.receiveChain));
          if (
            verificationFailure
            || channelStates.some((state) => !state.opened || !state.closed || state.receivedMessages !== nativeSizes.length)
          ) {
            throw new Error(verificationFailure ?? "browser_close_callbacks_incomplete");
          }
        },
        dispose(): void {
          peer.close();
        },
      };
      return { offerSdp };
    }, {
      browserMessages: encodeBase64(messages.browserToNative),
      nativeSizes: messages.nativeToBrowser.map((message) => message.byteLength),
      channels: QUALIFICATION_CHANNELS.map((channel) => ({ ...channel })),
      protocol: QUALIFICATION_DATA_CHANNEL_PROTOCOL,
      gatheringTimeoutMs: OFFER_GATHERING_TIMEOUT_MS,
      closeTimeoutMs: CLOSE_TIMEOUT_MS,
      packetBinding: packetBinding,
    }),
    "browser-offer",
    generation,
  );

  const offerFingerprint = sha256FingerprintFromSdp(bootstrap.offerSdp);
  if (!offerFingerprint) {
    throw qualificationFailure("browser-offer", generation, "browser_offer_fingerprint_missing");
  }

  return {
    offerSdp: bootstrap.offerSdp,
    offerFingerprint,
    acceptAnswer: async (answerSdp) => {
      await waitForQualification(
        "browser-answer",
        generation,
        CONNECTION_TIMEOUT_MS,
        runBrowserQualificationStep(
          page.evaluate(async (sdp) => window.__roostPeerQualification?.acceptAnswer(sdp) ?? Promise.reject(new Error("browser_state_missing")), answerSdp),
          "browser-answer",
          generation,
        ),
      );
    },
    waitForMessages: async () => await waitForQualification(
      "browser-messages",
      generation,
      CONNECTION_TIMEOUT_MS,
      runBrowserQualificationStep(
        page.evaluate(async () => window.__roostPeerQualification?.waitForMessages() ?? Promise.reject(new Error("browser_state_missing"))),
        "browser-messages",
        generation,
      ),
    ),
    waitForChannels: async () => await waitForQualification(
      "browser-channels",
      generation,
      CONNECTION_TIMEOUT_MS,
      runBrowserQualificationStep(
        page.evaluate(async () => window.__roostPeerQualification?.waitForChannels() ?? Promise.reject(new Error("browser_state_missing"))),
        "browser-channels",
        generation,
      ),
    ),
    sendPacket: async (lane, packet) => {
      const channel = QUALIFICATION_CHANNELS.find((candidate) => candidate.lane === lane);
      if (!channel) throw qualificationFailure("browser-packets", generation, "browser_packet_lane_missing");
      const encodedPacket = Buffer.from(packet.buffer, packet.byteOffset, packet.byteLength).toString("base64");
      await waitForQualification(
        "browser-packets",
        generation,
        CONNECTION_TIMEOUT_MS,
        runBrowserQualificationStep(
          page.evaluate(
            async ({ channelId, packet: encoded }) =>
              await (window.__roostPeerQualification?.sendPacket(channelId, encoded)
                ?? Promise.reject(new Error("browser_state_missing"))),
            { channelId: channel.id, packet: encodedPacket },
          ),
          "browser-packets",
          generation,
        ),
      );
    },
    close: async () => {
      await waitForQualification(
        "browser-close",
        generation,
        CLOSE_TIMEOUT_MS,
        runBrowserQualificationStep(
          page.evaluate(async () => window.__roostPeerQualification?.close() ?? Promise.reject(new Error("browser_state_missing"))),
          "browser-close",
          generation,
        ),
      );
    },
    dispose: async () => {
      await page.evaluate(() => window.__roostPeerQualification?.dispose()).catch(() => undefined);
    },
  };
}



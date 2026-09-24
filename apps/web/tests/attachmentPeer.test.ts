// Fake-browser WebRTC coverage for packetized attachment direct transport.
// It verifies static attachment channels, <=16 KiB packet sends, and ACK-driven completion.
// The fake never shares terminal packet helpers or a terminal peer connection.

import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES,
  ATTACHMENT_TRANSFER_PACKET_MAX_BYTES,
} from "@roost/protocol/attachment-transfer";
import {
  AttachmentTransferPacketAssembler,
  encodeAttachmentTransferPacket,
  type AttachmentTransferPacketQuota,
} from "@roost/protocol/attachment-transfer-packets";
import {
  AttachmentTransferAckSchema,
  AttachmentTransferClientFrameSchema,
  AttachmentTransferReadySchema,
  AttachmentTransferServerFrameSchema,
  type AttachmentTransferServerFrame,
} from "@roost/protocol/proto/attachment_transfer_pb";
import type { AttachmentDirectGrant } from "../src/lib/attachmentDirectGrant.ts";
import { openAttachmentPeerTransfer } from "../src/lib/attachmentPeer.ts";
import { sendAttachmentFile } from "../src/lib/attachmentTransfer.ts";

const SDP = [
  "v=0",
  "o=- 1 1 IN IP4 127.0.0.1",
  "s=-",
  "t=0 0",
  "a=ice-ufrag:test",
  "a=ice-pwd:0123456789012345678901",
  `a=fingerprint:sha-256 ${Array.from({ length: 32 }, () => "AA").join(":")}`,
  "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
  "a=setup:actpass",
  "a=sctp-port:5000",
  "a=max-message-size:65536",
  "a=candidate:1 1 udp 1 192.0.2.1 5000 typ host",
  "",
].join("\r\n");

class FakeDataChannel {
  readyState = "connecting";
  binaryType = "arraybuffer";
  bufferedAmount = 0;
  bufferedAmountLowThreshold = 0;
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onbufferedamountlow: (() => void) | null = null;
  onPacket: ((packet: Uint8Array) => void) | null = null;
  readonly sent: Uint8Array[] = [];

  constructor(readonly label: string, readonly options: RTCDataChannelInit) {}

  open(): void {
    this.readyState = "open";
    this.onopen?.();
  }

  send(bytes: BufferSource): void {
    const packet = bytes instanceof ArrayBuffer
      ? new Uint8Array(bytes)
      : new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const owned = new Uint8Array(packet);
    this.sent.push(owned);
    this.onPacket?.(owned);
  }

  deliver(packet: Uint8Array): void {
    this.onmessage?.({ data: packet.buffer.slice(packet.byteOffset, packet.byteOffset + packet.byteLength) });
  }

  close(): void {
    if (this.readyState === "closed") return;
    this.readyState = "closed";
    this.onclose?.();
  }
}

class FakePeerConnection {
  iceGatheringState = "complete";
  connectionState = "new";
  iceConnectionState = "new";
  localDescription: RTCSessionDescriptionInit | null = null;
  remoteDescription: RTCSessionDescriptionInit | null = null;
  ondatachannel: (() => void) | null = null;
  ontrack: (() => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  oniceconnectionstatechange: (() => void) | null = null;
  onicegatheringstatechange: (() => void) | null = null;
  readonly channels: FakeDataChannel[] = [];

  createDataChannel(label: string, options: RTCDataChannelInit): RTCDataChannel {
    const channel = new FakeDataChannel(label, options);
    this.channels.push(channel);
    return channel as unknown as RTCDataChannel;
  }

  createOffer(): Promise<RTCSessionDescriptionInit> {
    return Promise.resolve({ type: "offer", sdp: SDP });
  }

  setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = description;
    return Promise.resolve();
  }

  setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> {
    this.remoteDescription = description;
    return Promise.resolve();
  }

  close(): void {
    this.connectionState = "closed";
  }
}

const quota: AttachmentTransferPacketQuota = { reserve: () => true, release: () => undefined };

function grant(): AttachmentDirectGrant {
  return {
    workerFp: "worker-a",
    sessionId: "session-a",
    uploadId: "upload-a",
    filename: "peer.bin",
    shortPath: false,
    totalBytes: ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES,
    grantId: "grant-a",
    secret: "secret-a",
    tabId: "tab-a",
    deviceFingerprint: "device-a",
    workerEpoch: "epoch-a",
    peerSupported: true,
    stunUrls: [],
  };
}

function serverPacket(frame: AttachmentTransferServerFrame["frame"], messageId: number): Uint8Array {
  const bytes = toBinary(AttachmentTransferServerFrameSchema, create(AttachmentTransferServerFrameSchema, { frame }));
  return encodeAttachmentTransferPacket({ messageId, totalBytes: bytes.byteLength, offsetBytes: 0 }, bytes);
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 12; turn += 1) await Promise.resolve();
}

describe("openAttachmentPeerTransfer", () => {
  test("fragments attachment frames on separate ordered channels and completes from an ACK", async () => {
    const fake = new FakePeerConnection();
    const directGrant = grant();
    const opening = openAttachmentPeerTransfer(
      { grant: directGrant, peerId: "peer-a" },
      {
        createPeerConnection: () => fake as unknown as RTCPeerConnection,
        negotiate: async (request) => {
          expect(request).toMatchObject({
            workerFp: "worker-a",
            grantId: "grant-a",
            tabId: "tab-a",
            peerId: "peer-a",
            workerEpoch: "epoch-a",
          });
          return { peerId: "peer-a", answerSdp: SDP, workerEpoch: "epoch-a" };
        },
      },
    );
    await settle();
    const control = fake.channels[0];
    const data = fake.channels[1];
    if (!control || !data) throw new Error("attachment channels were not created");
    expect(fake.channels.map((channel) => [channel.label, channel.options.id, channel.options.negotiated, channel.options.ordered])).toEqual([
      ["roost-attachment-control-v1", 0, true, true],
      ["roost-attachment-data-v1", 1, true, true],
    ]);

    const outgoingControl = new AttachmentTransferPacketAssembler("incoming", quota);
    const outgoingData = new AttachmentTransferPacketAssembler("incoming", quota);
    let serverControlMessageId = 1;
    control.onPacket = (packet) => {
      const message = outgoingControl.push(packet);
      if (!message) return;
      const frame = fromBinary(AttachmentTransferClientFrameSchema, message).frame;
      if (frame.case !== "hello") throw new Error("expected attachment hello before Ready");
      expect(frame.value).toMatchObject({
        grantId: directGrant.grantId,
        secret: directGrant.secret,
        tabId: directGrant.tabId,
        deviceFingerprint: directGrant.deviceFingerprint,
        sessionId: directGrant.sessionId,
        uploadId: directGrant.uploadId,
        filename: directGrant.filename,
        shortPath: directGrant.shortPath,
        totalBytes: BigInt(directGrant.totalBytes),
        peerId: "peer-a",
        workerEpoch: directGrant.workerEpoch,
      });
      control.deliver(serverPacket({
        case: "ready",
        value: create(AttachmentTransferReadySchema, {
          workerFingerprint: "worker-a",
          workerEpoch: "epoch-a",
          sessionId: "session-a",
          uploadId: "upload-a",
        }),
      }, serverControlMessageId));
      serverControlMessageId += 1;
    };
    data.onPacket = (packet) => {
      const message = outgoingData.push(packet);
      if (!message) return;
      const frame = fromBinary(AttachmentTransferClientFrameSchema, message).frame;
      if (frame.case !== "chunk") throw new Error("expected attachment chunk");
      const chunk = frame.value;
      control.deliver(serverPacket({
        case: "ack",
        value: create(AttachmentTransferAckSchema, {
          uploadId: chunk.uploadId,
          seq: chunk.seq,
          bytesReceived: BigInt(chunk.data.byteLength),
          absPath: "/worker/peer.bin",
          error: "",
          chunkSha256: chunk.chunkSha256,
        }),
      }, serverControlMessageId));
      serverControlMessageId += 1;
    };
    control.open();
    data.open();
    const connection = await opening;
    const bytes = new Uint8Array(ATTACHMENT_TRANSFER_DIRECT_CHUNK_BYTES);
    bytes.fill(42);

    await expect(sendAttachmentFile({
      connection,
      sessionId: "session-a",
      uploadId: "upload-a",
      file: new File([bytes], "peer.bin"),
      readCoordinatorStatus: async () => { throw new Error("status was not needed"); },
    })).resolves.toEqual({ abs_path: "/worker/peer.bin" });

    expect(control.sent.every((packet) => packet.byteLength <= ATTACHMENT_TRANSFER_PACKET_MAX_BYTES)).toBe(true);
    expect(data.sent.every((packet) => packet.byteLength <= ATTACHMENT_TRANSFER_PACKET_MAX_BYTES)).toBe(true);
    expect(data.sent.length).toBeGreaterThan(1);
  });
});

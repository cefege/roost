// Carrier-selection coverage for one direct attachment upload.
// These fakes verify loopback fencing and the no-failover boundary without network I/O.
// Direct byte mechanics are covered separately by attachmentTransfer.test.ts.

import { describe, expect, mock, test } from "bun:test";
import {
  AttachmentTransferCarrierError,
  type AttachmentTransferAck,
  type AttachmentTransferChunk,
  type AttachmentTransferConnection,
  type AttachmentTransferStatus,
} from "../../../src/client/attachments/attachmentTransfer.ts";
import {
  uploadAttachmentDirect,
  type AttachmentDirectDependencies,
  type AttachmentDirectUploadRequest,
} from "../../../src/client/attachments/attachmentDirect.ts";
import type {
  AttachmentDirectGrant,
  AttachmentDirectGrantRequest,
} from "../../../src/client/attachments/attachmentDirectGrant.ts";

class AcceptedConnection implements AttachmentTransferConnection {
  sentChunk = false;

  async sendChunk(chunk: AttachmentTransferChunk): Promise<AttachmentTransferAck> {
    this.sentChunk = true;
    return {
      bytesReceived: Number(chunk.offset) + chunk.data.byteLength,
      absPath: chunk.last ? "/worker/direct.bin" : "",
      chunkSha256: chunk.chunkSha256,
    };
  }

  requestStatus(): Promise<AttachmentTransferStatus> {
    return Promise.reject(new Error("status was not needed"));
  }

  close(): void {}
}

function request(file = new File([new Uint8Array([1, 2])], "direct.bin")): AttachmentDirectUploadRequest {
  return {
    workerFp: "worker-a",
    sessionId: "session-a",
    uploadId: "upload-a",
    file,
    shortPath: false,
  };
}

function grant(request: AttachmentDirectGrantRequest): AttachmentDirectGrant {
  return {
    ...request,
    grantId: "grant-a",
    secret: "secret-a",
    tabId: "tab-a",
    deviceFingerprint: "device-a",
    workerEpoch: "epoch-a",
    peerSupported: true,
    stunUrls: [],
  };
}

function baseDependencies(): AttachmentDirectDependencies {
  return {
    readLocalWorkerDoor: () => null,
    mintGrant: async (directGrantRequest) => grant(directGrantRequest),
    openLoopback: async () => new AcceptedConnection(),
    openPeer: async () => new AcceptedConnection(),
    peerAvailable: () => true,
    createPeerId: () => "peer-a",
    readCoordinatorStatus: async () => { throw new Error("status was not needed"); },
  };
}

describe("uploadAttachmentDirect", () => {
  test("uses a matching loopback worker door before WebRTC", async () => {
    const upload = request();
    const calls: string[] = [];
    const mintedDescriptors: AttachmentDirectGrantRequest[] = [];
    const dependencies = {
      ...baseDependencies(),
      mintGrant: async (directGrantRequest: AttachmentDirectGrantRequest) => {
        mintedDescriptors.push(directGrantRequest);
        return grant(directGrantRequest);
      },
      readLocalWorkerDoor: () => ({ origin: "http://127.0.0.1:4104", workerFingerprint: "worker-a" }),
      openLoopback: async () => {
        calls.push("loopback");
        return new AcceptedConnection();
      },
      openPeer: async () => {
        calls.push("peer");
        return new AcceptedConnection();
      },
    };

    await expect(uploadAttachmentDirect(upload, dependencies)).resolves.toEqual({ abs_path: "/worker/direct.bin" });
    expect(calls).toEqual(["loopback"]);
    expect(mintedDescriptors).toEqual([{
      workerFp: "worker-a",
      sessionId: "session-a",
      uploadId: "upload-a",
      filename: "direct.bin",
      shortPath: false,
      totalBytes: 2,
    }]);
  });

  test("fences a mismatched door and advances a pre-send loopback failure to WebRTC", async () => {
    const upload = request();
    const calls: string[] = [];
    const openLoopback = async () => {
      calls.push("loopback");
      throw new AttachmentTransferCarrierError("loopback unavailable", false);
    };
    const openPeer = async () => {
      calls.push("peer");
      return new AcceptedConnection();
    };
    const mismatchedDoor = {
      ...baseDependencies(),
      readLocalWorkerDoor: () => ({ origin: "http://127.0.0.1:4104", workerFingerprint: "other-worker" }),
      openLoopback,
      openPeer,
    };

    await expect(uploadAttachmentDirect(upload, mismatchedDoor)).resolves.toEqual({ abs_path: "/worker/direct.bin" });
    expect(calls).toEqual(["peer"]);

    const matchingDoor = {
      ...mismatchedDoor,
      readLocalWorkerDoor: () => ({ origin: "http://127.0.0.1:4104", workerFingerprint: "worker-a" }),
    };
    await expect(uploadAttachmentDirect(upload, matchingDoor)).resolves.toEqual({ abs_path: "/worker/direct.bin" });
    expect(calls).toEqual(["peer", "loopback", "peer"]);
  });

  test("never switches to WebRTC after a loopback chunk was sent", async () => {
    const upload = request();
    const openPeer = mock(async () => new AcceptedConnection());
    const failedConnection: AttachmentTransferConnection = {
      sentChunk: false,
      async sendChunk() {
        throw new AttachmentTransferCarrierError("loopback failed after send", true);
      },
      requestStatus: async () => { throw new Error("status was not needed"); },
      close: () => undefined,
    };
    const dependencies = {
      ...baseDependencies(),
      readLocalWorkerDoor: () => ({ origin: "http://127.0.0.1:4104", workerFingerprint: "worker-a" }),
      openLoopback: async () => failedConnection,
      openPeer,
    };

    await expect(uploadAttachmentDirect(upload, dependencies)).rejects.toThrow("loopback failed after send");
    expect(openPeer).not.toHaveBeenCalled();
  });

  test("does not mint a direct grant when neither local carrier is possible", async () => {
    const upload = request();
    const mintGrant = mock(async (directGrantRequest: AttachmentDirectGrantRequest) => grant(directGrantRequest));
    const dependencies = {
      ...baseDependencies(),
      mintGrant,
      peerAvailable: () => false,
    };

    await expect(uploadAttachmentDirect(upload, dependencies)).resolves.toBeNull();
    expect(mintGrant).not.toHaveBeenCalled();
  });
});

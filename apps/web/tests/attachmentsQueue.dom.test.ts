// Native picker queue coverage — selected files appear immediately but start serially.
// A fake input preserves the user-gesture picker path without a browser DOM.
// Transfer calls are mocked so the test observes only queue state transitions.

import { describe, expect, mock, test } from "bun:test";
import type { Session } from "@roost/protocol/wire";
import type { Transfer } from "../src/store/transfers.ts";

interface FakeInput {
  type: string;
  multiple: boolean;
  style: { display: string };
  files: File[] | null;
  onchange: (() => void) | null;
  oncancel: (() => void) | null;
  click(): void;
  remove(): void;
}

let pickerInput: FakeInput | null = null;
const fakeDocument = {
  createElement: (tag: string) => {
    if (tag !== "input") throw new Error(`unexpected createElement(${tag})`);
    pickerInput = {
      type: "",
      multiple: false,
      style: { display: "" },
      files: null,
      onchange: null,
      oncancel: null,
      click: () => undefined,
      remove: () => undefined,
    };
    return pickerInput;
  },
  body: { appendChild: () => undefined },
};
Object.defineProperty(globalThis, "document", { configurable: true, value: fakeDocument });

const addTransfer = mock((_transfer: Pick<Transfer, "state">) => {});
const markTransferState = mock((_id: string, _state: string, _message?: string) => {});
const setTransferProgress = mock((_id: string, _bytesSent: number) => {});
const attachmentProbe = mock(async (request: { filename: string }) => ({
  hit: true,
  absPath: `/tmp/${request.filename}`,
}));

mock.module("../src/client/rpc/connect.ts", () => ({
  coordClient: { attachmentProbe, attachFileChunk: mock(() => undefined) },
}));
mock.module("../src/client/attachments/attachmentDirect.ts", () => ({ uploadAttachmentDirect: async () => null }));
mock.module("../src/lib/userTerminalInput.ts", () => ({ sendUserTerminalInput: mock(() => undefined) }));
mock.module("../src/store/transfers.ts", () => ({
  addTransfer,
  markTransferState,
  setTransferProgress,
}));

// Static import would bind the real transport before these module mocks install.
const { pickFilesTo } = await import("../src/lib/attachments.ts");
const session = { id: "session", channel: 1, worker_fp: "worker" } as unknown as Session;

describe("pickFilesTo upload queue", () => {
  test("marks later picker selections queued until their upload turn", async () => {
    const firstHashGate = Promise.withResolvers<void>();
    const firstHashStarted = Promise.withResolvers<void>();
    const secondHashGate = Promise.withResolvers<void>();
    const secondHashStarted = Promise.withResolvers<void>();
    const secondSunk = Promise.withResolvers<void>();
    const first = new File([new Uint8Array([1])], "first.png");
    const second = new File([new Uint8Array([2])], "second.mp4");
    const firstArrayBuffer = first.arrayBuffer.bind(first);
    const secondArrayBuffer = second.arrayBuffer.bind(second);
    Object.defineProperty(first, "arrayBuffer", {
      value: async () => {
        firstHashStarted.resolve();
        await firstHashGate.promise;
        return firstArrayBuffer();
      },
    });
    Object.defineProperty(second, "arrayBuffer", {
      value: async () => {
        secondHashStarted.resolve();
        await secondHashGate.promise;
        return secondArrayBuffer();
      },
    });

    pickFilesTo(session, (_path, file) => {
      if (file.name === second.name) secondSunk.resolve();
    });
    if (!pickerInput) throw new Error("picker input missing");
    pickerInput.files = [first, second];
    pickerInput.onchange?.();

    expect(addTransfer.mock.calls.map(([transfer]) => transfer.state))
      .toEqual(["queued", "queued"]);
    await firstHashStarted.promise;
    expect(markTransferState.mock.calls.map(([, state]) => state)).toEqual(["hashing"]);

    firstHashGate.resolve();
    await secondHashStarted.promise;
    expect(markTransferState.mock.calls.map(([, state]) => state))
      .toEqual(["hashing", "dedup", "hashing"]);

    secondHashGate.resolve();
    await secondSunk.promise;
  });
});

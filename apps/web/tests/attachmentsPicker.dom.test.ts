// Native-picker tripwire for lib/attachments.ts::pickFilesTo + injectPath.
//
// PickOptions is what routes a picker call to the rear camera vs the photo
// library vs the file browser, and none of that is observable except through
// the <input> the picker builds — hence this file.
//
// No jsdom/happy-dom (repo convention, see cellRenderer.dom.test.ts): a small
// fake covers exactly what the picker touches. The fake deliberately implements
// NO `capture` field, mirroring Chromium, where `capture` is declared in
// lib.dom but not implemented — so `input.capture = "environment"` lands as a
// dead expando the OS picker never sees. The first test asserts that absence,
// so the fake cannot quietly grow a `capture` property and retire the tripwire.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { Session } from "@roost/shared/wire";
import type { PickOptions } from "../src/lib/attachments.ts";
import { setRootStore } from "../src/store/root.ts";

interface FakeInput {
  type: string;
  multiple: boolean;
  accept: string;
  style: { display: string };
  onchange: (() => void) | null;
  oncancel: (() => void) | null;
  files: File[] | null;
  clicked: number;
  removed: boolean;
  removeCount: number;
  setAttribute(name: string, value: string): void;
  getAttribute(name: string): string | null;
  click(): void;
  remove(): void;
}

let created: FakeInput[] = [];
let appended: FakeInput[] = [];

function makeInput(): FakeInput {
  const attrs = new Map<string, string>();
  return {
    type: "",
    multiple: false,
    accept: "",
    style: { display: "" },
    onchange: null,
    oncancel: null,
    files: null,
    clicked: 0,
    removed: false,
    removeCount: 0,
    setAttribute(name, value) { attrs.set(name, value); },
    getAttribute(name) { return attrs.get(name) ?? null; },
    click() { this.clicked++; },
    remove() {
      this.removed = true;
      this.removeCount++;
      const index = appended.indexOf(this);
      if (index >= 0) appended.splice(index, 1);
    },
  };
}

Object.defineProperty(globalThis, "document", {
  configurable: true,
  value: {
    createElement: (tag: string) => {
      if (tag !== "input") throw new Error(`unexpected createElement(${tag})`);
      const el = makeInput();
      created.push(el);
      return el;
    },
    body: { appendChild: (el: FakeInput) => { appended.push(el); } },
  },
});

let inputSent = Promise.withResolvers<void>();
const sendInput = mock((_sessionId: string, _bytes: Uint8Array) => {
  inputSent.resolve();
  return {
    accepted: true as const,
    inputSeq: 1n,
    result: Promise.resolve({ status: "accepted" as const, inputSeq: 1n, writtenBytes: _bytes.byteLength }),
  };
});
const attachmentProbe = mock(async (_request: {
  sessionId: string;
  sha256: string;
  size: bigint;
  filename: string;
  shortPath: boolean;
}) => ({ hit: false, absPath: "" }));
const attachFileChunk = mock(async (request: {
  uploadId: string;
  sessionId: string;
  filename: string;
  shortPath: boolean;
  data: Uint8Array;
  last: boolean;
  seq: number;
}) => ({ absPath: request.last ? `/tmp/${request.filename}` : "" }));

mock.module("../src/connect.ts", () => ({ coordClient: { attachmentProbe, attachFileChunk } }));
mock.module("../src/lib/userTerminalInput.ts", () => ({ sendUserTerminalInput: sendInput }));
mock.module("../src/store/transfers.ts", () => ({
  addTransfer: mock(() => {}),
  markTransferState: mock(() => {}),
  setTransferProgress: mock(() => {}),
}));

// Dynamic import is REQUIRED: a static import binds the real transport modules
// before mock.module can replace them, dialing Connect/Sync at module load.
// The `import type` above is erased, so it does not defeat the mocks.
const {
  enqueueAttachment,
  enqueueAttachmentTo,
  injectPath,
  pickAndAttachFiles,
  pickFilesTo,
} = await import("../src/lib/attachments.ts");

const session = {
  id: "s1",
  channel: 1,
  worker_fp: "worker-linux",
} as unknown as Session;
const windowsSession = {
  id: "s-win",
  channel: 1,
  worker_fp: "worker-windows",
} as unknown as Session;

function pick(opts?: PickOptions): FakeInput {
  pickFilesTo(session, () => {}, opts);
  const el = created.at(-1);
  if (!el) throw new Error("pickFilesTo created no input");
  return el;
}


beforeEach(() => {
  created = [];
  appended = [];
  sendInput.mockClear();
  inputSent = Promise.withResolvers<void>();
  attachmentProbe.mockReset();
  attachmentProbe.mockImplementation(async () => ({ hit: false, absPath: "" }));
  attachFileChunk.mockReset();
  attachFileChunk.mockImplementation(async (request) => ({
    absPath: request.last ? `/tmp/${request.filename}` : "",
  }));
  setRootStore("workers", {});
});

afterEach(() => {
  setRootStore("workers", {});
});

describe("pickFilesTo", () => {
  test("capture lands on the ATTRIBUTE, not a dead IDL property", () => {
    // The gap this guards: `input.capture = opts.capture` typechecks and is
    // silently inert on Chromium, so "Take photo" opened the file browser.
    expect("capture" in makeInput()).toBe(false);
    expect(pick({ accept: "image/*", capture: "environment" }).getAttribute("capture"))
      .toBe("environment");
  });

  test("omitting capture leaves the attribute unset", () => {
    expect(pick({ accept: "image/*" }).getAttribute("capture")).toBeNull();
  });

  test("accept is set only when asked", () => {
    // `accept` IS a reflected IDL property in every engine, so the property
    // assignment is correct there — unlike `capture`.
    expect(pick({ accept: "image/*" }).accept).toBe("image/*");
    expect(pick().accept).toBe("");
  });

  test("multiple defaults on and is honored when disabled", () => {
    expect(pick().multiple).toBe(true);
    expect(pick({ multiple: false }).multiple).toBe(false);
  });

  test("the input is hidden, mounted, and clicked inside the caller's gesture", () => {
    // iOS only honors input.click() synchronously inside the user gesture, so
    // pickFilesTo must never defer any of this.
    const el = pick();
    expect(el.type).toBe("file");
    expect(el.style.display).toBe("none");
    expect(appended).toEqual([el]);
    expect(el.clicked).toBe(1);
  });

  test("change and cancel both remove the transient input exactly once", () => {
    const changed = pick();
    changed.files = [];
    changed.onchange?.();
    changed.oncancel?.();
    expect(changed.removed).toBe(true);
    expect(changed.removeCount).toBe(1);
    expect(appended).not.toContain(changed);

    const cancelled = pick();
    cancelled.oncancel?.();
    cancelled.onchange?.();
    expect(cancelled.removed).toBe(true);
    expect(cancelled.removeCount).toBe(1);
    expect(appended).not.toContain(cancelled);
  });

  test("the native picker still uploads and injects each chosen file", async () => {
    pickAndAttachFiles(session, { multiple: false });
    const el = created.at(-1);
    if (!el) throw new Error("pickAndAttachFiles created no input");
    el.files = [new File([new Uint8Array([1, 2, 3])], "picker's file.txt")];

    el.onchange?.();
    await inputSent.promise;

    expect(el.removed).toBe(true);
    expect(attachFileChunk).toHaveBeenCalled();
    expect(new TextDecoder().decode(sendInput.mock.calls[0]![1]))
      .toBe("'/tmp/picker'\"'\"'s file.txt' ");
  });

});

describe("enqueueAttachmentTo", () => {
  test("serializes hash through sink, preserves selection order, and reconstructs exact chunks", async () => {
    const chunkBytes = 4 * 1024 * 1024;
    const firstBytes = new Uint8Array(chunkBytes + 17);
    for (let i = 0; i < firstBytes.length; i++) firstBytes[i] = (i * 31 + 7) & 0xff;
    const first = new File([firstBytes], "first.bin", { type: "application/octet-stream" });
    const second = new File([new Uint8Array([9, 8, 7])], "second.bin");
    const events: string[] = [];
    const firstHashGate = Promise.withResolvers<void>();
    const firstArrayBuffer = first.arrayBuffer.bind(first);
    const secondArrayBuffer = second.arrayBuffer.bind(second);
    Object.defineProperty(first, "arrayBuffer", {
      value: async () => {
        events.push("hash:first");
        await firstHashGate.promise;
        return firstArrayBuffer();
      },
    });
    Object.defineProperty(second, "arrayBuffer", {
      value: async () => {
        events.push("hash:second");
        return secondArrayBuffer();
      },
    });

    attachmentProbe.mockImplementation(async (request) => {
      events.push(`probe:${request.filename}`);
      return request.filename === second.name
        ? { hit: true, absPath: "/tmp/second-dedup.bin" }
        : { hit: false, absPath: "" };
    });
    attachFileChunk.mockImplementation(async (request) => {
      events.push(`upload:${request.filename}:${request.seq}`);
      return { absPath: request.last ? "/tmp/first.bin" : "" };
    });

    const sinkOrder: string[] = [];
    const firstDone = enqueueAttachmentTo(session, first, (path) => {
      events.push("sink:first");
      sinkOrder.push(path);
    });
    const secondDone = enqueueAttachmentTo(session, second, (path) => {
      events.push("sink:second");
      sinkOrder.push(path);
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(events).toEqual(["hash:first"]);

    firstHashGate.resolve();
    await Promise.all([firstDone, secondDone]);

    expect(events).toEqual([
      "hash:first",
      "probe:first.bin",
      "upload:first.bin:0",
      "upload:first.bin:1",
      "sink:first",
      "hash:second",
      "probe:second.bin",
      "sink:second",
    ]);
    expect(sinkOrder).toEqual(["/tmp/first.bin", "/tmp/second-dedup.bin"]);

    const firstChunks = attachFileChunk.mock.calls.map(([request]) => request);
    expect(firstChunks.map(({ seq, last }) => ({ seq, last }))).toEqual([
      { seq: 0, last: false },
      { seq: 1, last: true },
    ]);
    const reconstructed = new Uint8Array(firstChunks.reduce((total, chunk) => total + chunk.data.length, 0));
    let offset = 0;
    for (const chunk of firstChunks) {
      reconstructed.set(chunk.data, offset);
      offset += chunk.data.length;
    }
    expect(reconstructed).toEqual(firstBytes);
  });

  test("failed upload never calls the sink", async () => {
    const sink = mock((_path: string, _file: File) => {});
    attachmentProbe.mockImplementation(async () => ({ hit: false, absPath: "" }));
    attachFileChunk.mockImplementation(async () => { throw new Error("upload failed"); });

    await enqueueAttachmentTo(session, new File([new Uint8Array([1, 2, 3])], "broken.bin"), sink);

    expect(sink).not.toHaveBeenCalled();
  });

  test("the default upload sink still injects the uploaded path", async () => {
    await enqueueAttachment(
      session,
      new File([new Uint8Array([1])], "drop $(literal).txt"),
    );

    expect(attachFileChunk).toHaveBeenCalled();
    expect(sendInput).toHaveBeenCalledTimes(1);
    expect(new TextDecoder().decode(sendInput.mock.calls[0]![1]))
      .toBe("'/tmp/drop $(literal).txt' ");
  });

  test("Windows still uploads but sends no terminal bytes", async () => {
    setRootStore("workers", {
      "worker-windows": { os: "win32" } as never,
    });

    await enqueueAttachment(
      windowsSession,
      new File([new Uint8Array([1])], "windows upload.txt"),
    );

    expect(attachFileChunk).toHaveBeenCalled();
    expect(sendInput).not.toHaveBeenCalled();
  });

});

describe("injectPath", () => {
  test("sends one quoted POSIX argument followed by exactly one ASCII space", () => {
    const cases = [
      ["/tmp/a b.txt", "'/tmp/a b.txt' "],
      ["/tmp/it's.txt", "'/tmp/it'\"'\"'s.txt' "],
      ["/tmp/$(printf exploited)", "'/tmp/$(printf exploited)' "],
      ["/tmp/`printf exploited`", "'/tmp/`printf exploited`' "],
      ["/tmp/你好 🐓.txt", "'/tmp/你好 🐓.txt' "],
    ] as const;

    for (const [absPath, expected] of cases) injectPath(session, absPath);

    expect(sendInput).toHaveBeenCalledTimes(cases.length);
    for (const [index, [, expected]] of cases.entries()) {
      const [sessionId, bytes] = sendInput.mock.calls[index]!;
      expect(sessionId).toBe("s1");
      expect(new TextDecoder().decode(bytes)).toBe(expected);
      expect(bytes.at(-1)).toBe(0x20);
      expect(bytes.includes(0x0a)).toBe(false);
      expect(bytes.includes(0x0d)).toBe(false);
    }
  });

  test("sends nothing for every C0, DEL, and C1 control character", () => {
    const controls = [
      ...Array.from({ length: 0x20 }, (_, codePoint) => codePoint),
      ...Array.from({ length: 0x21 }, (_, offset) => 0x7f + offset),
    ];

    for (const codePoint of controls) {
      injectPath(session, `/tmp/before${String.fromCodePoint(codePoint)}after`);
    }

    expect(sendInput).not.toHaveBeenCalled();
  });

  test("uses the session worker platform and sends nothing on Windows", () => {
    setRootStore("workers", {
      "worker-windows": { os: "win32" } as never,
    });

    injectPath(windowsSession, "/posix-looking/path.txt");

    expect(sendInput).not.toHaveBeenCalled();
  });
});

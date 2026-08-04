// Native-picker tripwire for lib/attachments.ts::pickFilesTo + injectPath.
//
// PickOptions is what routes the composer's "+" sheet to the rear camera vs the
// photo library vs the file browser, and none of that is observable except
// through the <input> the picker builds — hence this file.
//
// No jsdom/happy-dom (repo convention, see cellRenderer.dom.test.ts): a small
// fake covers exactly what the picker touches. The fake deliberately implements
// NO `capture` field, mirroring Chromium, where `capture` is declared in
// lib.dom but not implemented — so `input.capture = "environment"` lands as a
// dead expando the OS picker never sees. The first test asserts that absence,
// so the fake cannot quietly grow a `capture` property and retire the tripwire.

import { describe, test, expect, mock, beforeEach } from "bun:test";
import type { Session } from "@roost/shared/wire";
import type { PickOptions } from "../src/lib/attachments.ts";

interface FakeInput {
  type: string;
  multiple: boolean;
  accept: string;
  style: { display: string };
  onchange: (() => void) | null;
  files: null;
  clicked: number;
  removed: boolean;
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
    files: null,
    clicked: 0,
    removed: false,
    setAttribute(name, value) { attrs.set(name, value); },
    getAttribute(name) { return attrs.get(name) ?? null; },
    click() { this.clicked++; },
    remove() { this.removed = true; },
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

const sendInput = mock((_sessionId: string, _bytes: Uint8Array) => {});

mock.module("../src/connect.ts", () => ({ coordClient: {} }));
mock.module("../src/ws/input-channel.ts", () => ({ inputChannel: { sendInput } }));
mock.module("../src/store/transfers.ts", () => ({
  addTransfer: mock(() => {}),
  markTransferState: mock(() => {}),
  setTransferProgress: mock(() => {}),
}));

// Dynamic import is REQUIRED: a static import binds the real ../src/connect.ts
// and input-channel.ts before mock.module can replace them, dialing a Connect
// transport at module load. The `import type` above is erased, so it does not
// defeat the mocks.
const { pickFilesTo, injectPath } = await import("../src/lib/attachments.ts");

const session = { id: "s1", channel: 1 } as unknown as Session;

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
});

describe("injectPath", () => {
  test("types the absolute path into the PTY with one trailing space", () => {
    injectPath(session, "/tmp/a b.txt");
    expect(sendInput).toHaveBeenCalledTimes(1);
    const [sessionId, bytes] = sendInput.mock.calls[0]!;
    expect(sessionId).toBe("s1");
    expect(new TextDecoder().decode(bytes)).toBe("/tmp/a b.txt ");
  });
});

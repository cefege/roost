// "New browser paired" notice: one toast per pairing however many signals
// report it, a label built from the parsed provenance, and the Sync PairCompleted
// delta that drops the approved card and announces through the same notice.

import { create } from "@bufbuild/protobuf";
import { PairCompletedSchema, PairRequestDeltaProtoSchema } from "@roost/protocol/proto/events_pb";
import { FirehoseFrameSchema } from "@roost/protocol/proto/sync_pb";
import { beforeEach, describe, expect, mock, test } from "bun:test";

const toasts: string[] = [];
mock.module("../src/store/toastStore.ts", () => ({
  addToast: (message: string) => {
    toasts.push(message);
    return () => undefined;
  },
}));

// The notice and store graph bind the toast mock at evaluation
// (module-loading boundary).
const { announcePairedBrowser, formatPairedBrowserLabel } = await import("../src/lib/pairedBrowserNotice.ts");
const { rootStore, setRootStore } = await import("../src/store/root.ts");
const { _dispatchSyncFrame } = await import("../src/store/sync-frame.ts");

const DESCRIPTION = {
  label: "Chrome — macOS",
  clientBrowser: "Chrome",
  clientOs: "macOS",
  city: "Berlin",
  region: "Berlin",
  countryCode: "DE",
};

beforeEach(() => {
  toasts.length = 0;
});

describe("announcePairedBrowser", () => {
  test("announces each pairing once however many signals report it", () => {
    announcePairedBrowser({ ephemeralId: "a".repeat(32), label: "Kitchen tablet" });
    announcePairedBrowser({ ephemeralId: "a".repeat(32), label: "Chrome on macOS · Berlin" });
    announcePairedBrowser({ ephemeralId: "b".repeat(32), label: "Office laptop" });
    expect(toasts).toEqual([
      "New browser paired: Kitchen tablet",
      "New browser paired: Office laptop",
    ]);
  });
});

describe("formatPairedBrowserLabel", () => {
  test("names the parsed browser and OS with the most specific known place", () => {
    expect(formatPairedBrowserLabel(DESCRIPTION)).toBe("Chrome on macOS · Berlin");
    expect(formatPairedBrowserLabel({ ...DESCRIPTION, city: " ", region: "" })).toBe("Chrome on macOS · DE");
    expect(formatPairedBrowserLabel({ ...DESCRIPTION, city: "", region: "", countryCode: "" }))
      .toBe("Chrome on macOS");
  });

  test("falls back to the requester label when no browser or OS was parsed", () => {
    expect(formatPairedBrowserLabel({ ...DESCRIPTION, clientBrowser: "", clientOs: "", city: "" }))
      .toBe("Chrome — macOS · Berlin");
    expect(formatPairedBrowserLabel({ ...DESCRIPTION, clientOs: "" })).toBe("Chrome · Berlin");
  });
});

describe("Sync PairCompleted delta", () => {
  test("drops the approved card and announces the new browser once", () => {
    const ephemeralId = "c".repeat(32);
    setRootStore("pair_requests", ephemeralId, { ephemeral_id: ephemeralId, label: "Chrome — macOS", created_at_ms: 1 });
    const frame = create(FirehoseFrameSchema, {
      frame: {
        case: "pairRequestDelta",
        value: create(PairRequestDeltaProtoSchema, {
          kind: {
            case: "completed",
            value: create(PairCompletedSchema, { ...DESCRIPTION, ephemeralId, pairedAtMs: 1n }),
          },
        }),
      },
    });
    expect(_dispatchSyncFrame(frame)).toBe(true);
    expect(_dispatchSyncFrame(frame)).toBe(true);
    expect(rootStore.pair_requests[ephemeralId]).toBeUndefined();
    expect(toasts).toEqual(["New browser paired: Chrome on macOS · Berlin"]);
  });
});

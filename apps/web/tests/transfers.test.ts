// transfers.test.ts — the unified transfer store: EMA speed/ETA math, terminal
// state transitions (auto-dismiss vs persist), and the late-callback no-op.
// Drives the real module-level store; every test uses a unique id so the global
// singleton can't cross-contaminate.

import { expect, test, describe, vi } from "bun:test";
import {
  transfers, addTransfer, clearTransfersForLogout, setTransferProgress, markTransferState, removeTransfer,
} from "../src/store/transfers.ts";

describe("transfers store", () => {
  test("speed + ETA derive from timed progress ticks", () => {
    const id = crypto.randomUUID();
    const MiB = 1024 * 1024;
    addTransfer({ id, name: "big.bin", dir: "up", bytes_total: 8 * MiB, state: "active" });
    setTransferProgress(id, 0, undefined, 1000);          // seed the first sample
    setTransferProgress(id, 4 * MiB, undefined, 2000);    // +4 MiB in 1.0s → 4 MiB/s

    const t = transfers[id]!;
    expect(t.speed).toBeCloseTo(4 * MiB, -3);             // ~4 MiB/s
    expect(t.eta_s).toBeCloseTo(1, 5);                    // 4 MiB left / 4 MiB/s = 1s
    removeTransfer(id);
  });

  test("sub-50ms ticks accumulate bytes but don't produce a noisy rate", () => {
    const id = crypto.randomUUID();
    addTransfer({ id, name: "x", dir: "down", bytes_total: 1000, state: "active" });
    setTransferProgress(id, 0, undefined, 1000);
    setTransferProgress(id, 500, undefined, 1010);        // +10ms → below MIN_DELTA_S
    const t = transfers[id]!;
    expect(t.bytes_done).toBe(500);
    expect(t.speed).toBe(0);                              // no rate from a too-close tick
    removeTransfer(id);
  });

  test("ok auto-dismisses after 2s; err persists", () => {
    vi.useFakeTimers();
    try {
      const idOk = crypto.randomUUID();
      addTransfer({ id: idOk, name: "a", dir: "up", bytes_total: 10, state: "active" });
      markTransferState(idOk, "ok");
      expect(transfers[idOk]?.state).toBe("ok");
      vi.advanceTimersByTime(2001);
      expect(transfers[idOk]).toBeUndefined();

      const idErr = crypto.randomUUID();
      addTransfer({ id: idErr, name: "b", dir: "up", bytes_total: 10, state: "active" });
      markTransferState(idErr, "err", "boom");
      vi.advanceTimersByTime(10_000);
      expect(transfers[idErr]?.state).toBe("err");
      expect(transfers[idErr]?.err).toBe("boom");
      removeTransfer(idErr);
    } finally {
      vi.useRealTimers();
    }
  });

  test("dedup auto-dismisses like ok", () => {
    vi.useFakeTimers();
    try {
      const id = crypto.randomUUID();
      addTransfer({ id, name: "c", dir: "up", bytes_total: 10, state: "hashing" });
      markTransferState(id, "dedup");
      expect(transfers[id]?.state).toBe("dedup");
      vi.advanceTimersByTime(2001);
      expect(transfers[id]).toBeUndefined();
    } finally {
      vi.useRealTimers();
    }
  });

  test("progress after removal is a no-op (can't resurrect)", () => {
    const id = crypto.randomUUID();
    addTransfer({ id, name: "d", dir: "down", bytes_total: 100, state: "active" });
    removeTransfer(id);
    setTransferProgress(id, 50, 100, 1000);
    expect(transfers[id]).toBeUndefined();
  });

  test("releases each preview URL once across transfer lifecycles", () => {
    const originalRevokeObjectURL = URL.revokeObjectURL;
    const revokedUrls: string[] = [];
    URL.revokeObjectURL = (url) => { revokedUrls.push(url); };
    vi.useFakeTimers();
    try {
      addTransfer({ id: "replacement", name: "first.png", dir: "up", bytes_total: 1, state: "queued", preview_url: "blob:first" });
      addTransfer({ id: "replacement", name: "second.png", dir: "up", bytes_total: 1, state: "queued", preview_url: "blob:second" });
      removeTransfer("replacement");
      removeTransfer("replacement");

      addTransfer({ id: "replacement-no-preview", name: "photo.png", dir: "up", bytes_total: 1, state: "queued", preview_url: "blob:no-preview" });
      addTransfer({ id: "replacement-no-preview", name: "notes.txt", dir: "up", bytes_total: 1, state: "queued" });
      expect(transfers["replacement-no-preview"]?.preview_url).toBeUndefined();
      removeTransfer("replacement-no-preview");

      addTransfer({ id: "success", name: "success.png", dir: "up", bytes_total: 1, state: "active", preview_url: "blob:success" });
      markTransferState("success", "ok");
      addTransfer({ id: "dedup", name: "dedup.png", dir: "up", bytes_total: 1, state: "hashing", preview_url: "blob:dedup" });
      markTransferState("dedup", "dedup");
      vi.advanceTimersByTime(2001);

      addTransfer({ id: "logout-one", name: "one.png", dir: "up", bytes_total: 1, state: "err", preview_url: "blob:logout-one" });
      addTransfer({ id: "logout-two", name: "two.png", dir: "up", bytes_total: 1, state: "err", preview_url: "blob:logout-two" });
      clearTransfersForLogout();

      expect(revokedUrls).toEqual([
        "blob:first",
        "blob:second",
        "blob:no-preview",
        "blob:success",
        "blob:dedup",
        "blob:logout-one",
        "blob:logout-two",
      ]);
    } finally {
      clearTransfersForLogout();
      vi.useRealTimers();
      URL.revokeObjectURL = originalRevokeObjectURL;
    }
  });
});

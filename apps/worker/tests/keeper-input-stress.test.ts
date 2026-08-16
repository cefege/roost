// Byte-fidelity stress test for the worker→keeper→PTY input path.
//
// Drives MultiplexedKeeperPool directly (no SPA, no coord) with `cat`
// as the child process. cat is a perfect byte-for-byte echo, so any
// substitution in the keeper's PtyIn handler shows up as a diff
// between bytes-sent and bytes-received.
//
// Bug class this guards: 2026-06-17 "backspace acts like space" —
// `f.payload` was a subarray view onto the keeper's receive buffer;
// Bun's `proc.terminal.write` queued the write async; receive buffer
// rolled before the write flushed; PTY read whichever byte overwrote
// the slot. Symptom matrix was: single keystroke control bytes
// (Backspace=0x7f, Tab=0x09, Esc=0x1b) all at risk. CSI sequences
// (arrow keys = 3-byte ESC[A etc.) at risk for any byte in the run.
// Paste bursts at MAXIMUM risk because of the back-to-back frame
// pattern that triggers receive-buffer roll mid-write.
//
// Scenarios in this file cover one byte per failure category. New
// failure modes get a new test case, not a bigger existing test.

import { describe, test, expect, afterAll } from "bun:test";
import { rmSync } from "node:fs";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { MultiplexedKeeperPool, type MuxChannelCallbacks } from "../src/keeper/multiplexed-client.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";

afterAll(() => {
  // This file owns a PRIVATE pool (not the getMultiplexedPool() singleton) on
  // its own SOCK_DIR, so the keeper it spawned is ours alone to reap. dispose()
  // deliberately leaves it running (prod keepers outlive worker restarts), so
  // without this every suite run left a keeper alive forever.
  const keeperPid = pool._keeperProc?.pid;
  pool.dispose();
  if (keeperPid) try { process.kill(keeperPid, "SIGKILL"); } catch { /* already dead */ }
  // Whole dir: unlinking only the socket left SOCK_DIR in $TMPDIR every run.
  rmSync(SOCK_DIR, { recursive: true, force: true });
});

// One pool per test file — keeper subprocess + UDS reused across cases.
const pool = new MultiplexedKeeperPool();

let _nextCh = 100;

/** Spawn `sh -c 'stty raw -echo; exec cat'` on a fresh channel so the
 *  PTY slave is in raw-passthrough mode: every byte we send echoes
 *  back via cat, byte-for-byte, no line-discipline interference. This
 *  is what we want for testing the worker→keeper→PTY input path —
 *  any byte substitution between us and cat's stdout shows up as a
 *  diff. */
async function spawnRawCat(): Promise<{
  channelId: number;
  send: (bytes: Uint8Array | string) => void;
  drain: (idleMs: number) => Promise<Buffer>;
  kill: () => Promise<number | null>;
}> {
  const channelId = _nextCh++;
  let captured = Buffer.alloc(0);
  let exitCode: number | null = null;
  let exitResolve: ((c: number | null) => void) | null = null;
  const cb: MuxChannelCallbacks = {
    onOutput: (chunk) => { captured = Buffer.concat([captured, chunk]); },
    onExit: (code) => { exitCode = code; exitResolve?.(code); exitResolve = null; },
    onError: () => { /* tests don't care */ },
  };
  await pool.spawn({
    channelId,
    shellSpec: keeperTestShellSpec({
      executable: "/bin/sh",
      argv: ["-c", "stty raw -echo 2>/dev/null; exec /bin/cat"],
      cwd: homedir(),
    }),
    cols: 200, rows: 50,
    callbacks: cb,
  });
  // Let sh fork + exec cat + reach its read loop.
  await Bun.sleep(150);
  // Discard whatever stderr/early bytes appeared during boot.
  captured = Buffer.alloc(0);

  return {
    channelId,
    send: (bytes) => {
      const u = typeof bytes === "string"
        ? new TextEncoder().encode(bytes)
        : (bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
      pool.input(channelId, u);
    },
    drain: async (idleMs: number) => {
      let last = captured.length;
      let stable = 0;
      const start = Date.now();
      while (Date.now() - start < 2000) {
        await Bun.sleep(20);
        if (captured.length === last) stable += 20;
        else { stable = 0; last = captured.length; }
        if (stable >= idleMs) break;
      }
      const out = captured;
      captured = Buffer.alloc(0);  // reset for next drain
      return out;
    },
    kill: async () => {
      pool.kill(channelId);
      if (exitCode !== null) return exitCode;
      return new Promise<number | null>((resolve) => {
        exitResolve = resolve;
        setTimeout(() => resolve(null), 500);
      });
    },
  };
}

// In PTY cooked-mode echo, the slave echoes input bytes back (modulo
// some transformations for control chars). For our hex-strict comparisons
// we want raw echo. `stty raw` would do it, but cat is the running process
// and can't issue stty. Workaround: send the input byte AND a newline,
// then check that the input byte appears in the echo stream verbatim
// before the newline-driven flush. For control chars (which the default
// termios echoes as ^X-style), we look at the FIRST occurrence of the
// expected hex pattern in the captured stream.
//
// Simpler still: use Bun.spawn directly with a raw-mode helper inside
// the PTY. But the whole point is to test the keeper's PtyIn path, so
// we keep cat and the default termios. We assert on the FIRST instance
// of our payload (input echo) showing up in the echo stream.

function _hexHas(haystack: Buffer, needle: Uint8Array): boolean {
  if (needle.length === 0) return true;
  outer: for (let i = 0; i <= haystack.length - needle.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer;
    }
    return true;
  }
  return false;
}

// In raw mode, every byte we send shows up exactly once in cat's
// output. We assert byte-equality between sent and received.
async function sendAndExpect(payload: Uint8Array): Promise<void> {
  const cat = await spawnRawCat();
  cat.send(payload);
  const out = await cat.drain(200);
  // Strict equality: every byte sent appears in the output, in order,
  // with the same length. No substitutions, no drops.
  if (out.length !== payload.length) {
    // Surface the diff for debugging.
    const sentHex = Array.from(payload).map(b => b.toString(16).padStart(2, "0")).join(" ");
    const gotHex = Array.from(out).map(b => b.toString(16).padStart(2, "0")).join(" ");
    throw new Error(`length mismatch sent=${payload.length} got=${out.length}\n  sent: ${sentHex}\n  got:  ${gotHex}`);
  }
  for (let i = 0; i < payload.length; i++) {
    if (out[i] !== payload[i]) {
      const sentHex = Array.from(payload).map(b => b.toString(16).padStart(2, "0")).join(" ");
      const gotHex = Array.from(out).map(b => b.toString(16).padStart(2, "0")).join(" ");
      throw new Error(`byte ${i} mismatch sent=0x${payload[i].toString(16)} got=0x${out[i].toString(16)}\n  sent: ${sentHex}\n  got:  ${gotHex}`);
    }
  }
  await cat.kill();
}

describe("keeper PtyIn byte fidelity (the 'backspace acts like space' guard)", () => {
  test("Backspace byte (0x7f) round-trips intact — pre-fix this became 0x20", async () => {
    await sendAndExpect(new Uint8Array([0x7f]));
  });

  test("Tab byte (0x09) round-trips intact", async () => {
    await sendAndExpect(new Uint8Array([0x09]));
  });

  test("ESC byte (0x1b) round-trips intact", async () => {
    await sendAndExpect(new Uint8Array([0x1b]));
  });

  test("Bell byte (0x07) round-trips intact", async () => {
    await sendAndExpect(new Uint8Array([0x07]));
  });

  test("NUL byte (0x00) round-trips intact", async () => {
    await sendAndExpect(new Uint8Array([0x00]));
  });

  test("CSI Up arrow (ESC [ A) — 3-byte sequence intact + ordered", async () => {
    await sendAndExpect(new Uint8Array([0x1b, 0x5b, 0x41]));
  });

  test("CSI Down arrow (ESC [ B)", async () => {
    await sendAndExpect(new Uint8Array([0x1b, 0x5b, 0x42]));
  });

  test("CSI Left arrow (ESC [ D)", async () => {
    await sendAndExpect(new Uint8Array([0x1b, 0x5b, 0x44]));
  });

  test("CSI Right arrow (ESC [ C)", async () => {
    await sendAndExpect(new Uint8Array([0x1b, 0x5b, 0x43]));
  });

  test("F1 function key (ESC O P) — alternative SS3 form", async () => {
    await sendAndExpect(new Uint8Array([0x1b, 0x4f, 0x50]));
  });

  test("Cmd-Backspace = NAK (0x15) — wterm's mapping for kill-line", async () => {
    await sendAndExpect(new Uint8Array([0x15]));
  });

  test("Ctrl-C = ETX (0x03)", async () => {
    await sendAndExpect(new Uint8Array([0x03]));
  });

  test("Ctrl-D = EOT (0x04)", async () => {
    await sendAndExpect(new Uint8Array([0x04]));
  });

  test("Ctrl-L = FF (0x0c)", async () => {
    await sendAndExpect(new Uint8Array([0x0c]));
  });

  test("Ctrl-W = ETB (0x17) — delete-word", async () => {
    await sendAndExpect(new Uint8Array([0x17]));
  });

  test("UTF-8 emoji 🔥 (4 bytes f0 9f 94 a5)", async () => {
    await sendAndExpect(new Uint8Array([0xf0, 0x9f, 0x94, 0xa5]));
  });

  test("UTF-8 CJK 汉 (3 bytes e6 b1 89)", async () => {
    await sendAndExpect(new Uint8Array([0xe6, 0xb1, 0x89]));
  });

  test("UTF-8 Latin1 é (2 bytes c3 a9)", async () => {
    await sendAndExpect(new Uint8Array([0xc3, 0xa9]));
  });

  test("4 KB ASCII paste — the canonical paste-burst stress", async () => {
    const payload = new Uint8Array(4096);
    for (let i = 0; i < payload.length; i++) payload[i] = 0x41 + (i % 26);
    await sendAndExpect(payload);
  });

  test("interleaved Cmd-Backspace + text + Backspace + arrow + char", async () => {
    await sendAndExpect(new Uint8Array([
      0x15,                          // Cmd-Backspace
      0x68, 0x65, 0x6c, 0x6c, 0x6f,  // "hello"
      0x7f,                          // Backspace
      0x1b, 0x5b, 0x44,              // Left arrow
      0x58,                          // X
    ]));
  });

  test("100 back-to-back single-byte 0x7f frames — the original repro", async () => {
    const cat = await spawnRawCat();
    // Pre-fix: many of these became 0x20 because the receive buffer
    // rolled before Bun's async terminal.write flushed.
    for (let i = 0; i < 100; i++) cat.send(new Uint8Array([0x7f]));
    const out = await cat.drain(400);
    // Every byte must be 0x7f exactly. No substitution allowed.
    expect(out.length).toBe(100);
    let allDel = true;
    for (let i = 0; i < out.length; i++) if (out[i] !== 0x7f) { allDel = false; break; }
    expect(allDel).toBe(true);
    await cat.kill();
  });

  test("256-frame burst of every byte 0x00..0xff — exhaustive byte coverage", async () => {
    const cat = await spawnRawCat();
    for (let b = 0; b < 256; b++) cat.send(new Uint8Array([b]));
    const out = await cat.drain(500);
    expect(out.length).toBe(256);
    for (let b = 0; b < 256; b++) {
      if (out[b] !== b) throw new Error(`byte ${b} corrupted: got 0x${out[b].toString(16)}`);
    }
    await cat.kill();
  });
});

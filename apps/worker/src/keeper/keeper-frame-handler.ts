// Mux frame dispatch for the multiplexed keeper. Split out of
// multiplexed-main.ts. The entry owns the single `channels` Map + `broadcast`
// and the zsh ZDOTDIR override, and passes them in via FrameHandlerCtx so
// there is exactly one source of truth for keeper state.

import {
  MuxFrameType,
  KEEPER_PROTOCOL_VERSION,
  encodeMuxFrame,
  decodeSpawnRequest,
} from "./protocol-v2.ts";
import { KEEPER_BUILD_STAMP } from "./keeper-stamp.ts";
import { _log, _keeperOpenFdCount } from "./keeper-log.ts";
import { reapChannelTree } from "./keeper-process-reap.ts";
import type { Channel, ClientState } from "./keeper-types.ts";
import { createSbRing, appendToRing, readRing } from "../session-scrollback-ring.ts";

// RC2: per-channel output ring kept on the keeper so head_seq + history
// survive a worker restart (the keeper outlives the worker). Matches the
// worker-side SCROLLBACK_CAP_BYTES so a resume re-seeds the same depth.
// 2026-06-22: 8 MB → 1 MB. The keeper is the jetsam victim on a permanently
// RAM-full box (detached child, low jetsam band); 12 channels × 8 MB = 96 MB
// worst-case made it a fat target. 1 MB/ch (~10k lines) keeps generous depth
// while cutting the worst-case footprint to 12 MB. See memory
// project_keeper_death_auto_respawn (jetsam root cause).
const KEEPER_RING_CAP_BYTES = 1 * 1024 * 1024;

// GetHistory on an unknown/exited channel: no ring to read.
const EMPTY_U8 = new Uint8Array(0);

export interface FrameHandlerCtx {
  channels: Map<number, Channel>;
  broadcast: (frame: Buffer) => void;
  ZSH_OVERRIDE_DIR: string;
  BASH_RC_PATH: string;
}

export function handleFrame(ctx: FrameHandlerCtx, client: ClientState, f: { type: MuxFrameType; channelId: number; payload: Buffer }): void {
  const { channels, broadcast, ZSH_OVERRIDE_DIR, BASH_RC_PATH } = ctx;
  switch (f.type) {
    case MuxFrameType.Spawn: {
      const req = decodeSpawnRequest(f.payload);
      if (!req) {
        // Malformed Spawn frame — previously a silent return, so a wire/codec
        // mismatch looked like "spawn did nothing". Log it.
        _log("error", "multiplexed-keeper", "spawn_decode_failed", { payload_len: f.payload.length });
        return;
      }
      if (channels.has(req.channel_id)) {
        _log("warn", "multiplexed-keeper", "spawn_channel_in_use", { channelId: req.channel_id });
        client.socket.write(encodeMuxFrame(MuxFrameType.SpawnErr, req.channel_id, JSON.stringify({ error: "channel_id in use" })));
        return;
      }
      // A plain-shell spawn (session-spawn.ts / session-resume.ts) sends a
      // one-element argv; anything a caller built itself is spawned verbatim.
      const plainShell = (req.argv?.length ?? 0) <= 1;
      const shell = req.argv?.[0] ?? process.env.SHELL ?? "/bin/sh";
      // isZsh keeps its historical last-token test so an explicit
      // ["/bin/zsh","-lc",cmd] doesn't get the interactive ZDOTDIR override.
      const isZsh = /(^|\/)zsh$/.test((req.argv ?? [shell]).at(-1) ?? "");
      const isBash = /(^|\/)bash$/.test(shell);
      // bash has no ZDOTDIR: point it at the roost rcfile, which sources the
      // user's ~/.bashrc and then installs the OSC 7 PROMPT_COMMAND hook.
      const argv = plainShell
        ? (isBash ? [shell, "--rcfile", BASH_RC_PATH] : [shell])
        : req.argv!;
      const spawnedAtMs = Date.now();
      try {
        // ZDOTDIR points zsh at our override .zshrc which unsetopts
        // PROMPT_SP / PROMPT_CR + clears PROMPT_EOL_MARK then sources
        // the user's real ~/.zshrc. TERM_PROGRAM=Apple_Terminal
        // triggers /etc/zshrc_Apple_Terminal so the chpwd hook emits
        // OSC 7 for cwd tracking (see session-manager._scanOsc7).
        const proc = Bun.spawn(argv, {
          cwd: req.cwd,
          terminal: {
            cols: req.cols, rows: req.rows,
            data: (_t, data) => {
              // Defensive COPY: `data` is a Uint8Array view that Bun's
              // PTY callback may reuse for the next chunk. `Buffer.from
              // (data.buffer, byteOffset, byteLength)` would alias the
              // same memory; a back-to-back data() in the same tick
              // would let the second call overwrite the first before
              // broadcast() finishes encoding. Symmetric with the
              // defensive copy on the PtyIn path below. See CLAUDE.md
              // L11 "BufferSource async-write semantics" row.
              const buf = Buffer.from(data);
              // RC2: retain in the per-channel ring + advance head_seq
              // BEFORE broadcast so the keeper's count matches what every
              // worker (live + post-restart) sees. channels.get is set
              // synchronously after Bun.spawn returns; this async PTY
              // callback fires on a later tick, so `c` is always present.
              const c = channels.get(req.channel_id);
              if (c) {
                c.headSeq += buf.length;
                appendToRing(c.outRing, buf);
              }
              broadcast(encodeMuxFrame(MuxFrameType.PtyOut, req.channel_id, buf));
            },
          },
          env: {
            ...process.env,
            // Explicit TERM + LANG defaults. Bun.spawn's `terminal:`
            // option sets the PTY's internal name but does NOT inject
            // TERM into the spawned child's env (node-pty did this
            // automatically — Bun doesn't). When the worker runs under
            // launchd via SSH-bootstrapped LaunchAgent (no inherited
            // TTY env), process.env.TERM is unset and the spawned shell
            // sees TERM="" or "unknown" → ncurses fails ("cannot
            // initialize terminal type"), zsh's zle falls back to a
            // degenerate display path where backward-delete-char emits
            // just 0x20 instead of 0x08 0x20 0x08, and Cmd-Backspace
            // (kill-line) blows the entire row away because the el/ed
            // terminfo caps can't be looked up. Setting TERM here ahead
            // of the spread fixes the entire class of symptoms.
            // Repro 2026-06-17: luci/m5 deployments showed all three;
            // local m1 (originally bootstrapped from Terminal.app)
            // inherited TERM through launchd and hid the bug.
            TERM: "xterm-256color",
            // Truecolor isn't in the xterm-256color terminfo entry. Modern
            // TUIs check COLORTERM=truecolor and emit 24-bit SGR directly,
            // which wterm renders natively; otherwise they quantize to 256
            // colors on a true-color display.
            COLORTERM: "truecolor",
            // macOS ships en_US.UTF-8; a stock Linux box often has only
            // C.UTF-8, and an unknown LANG makes glibc fall back to POSIX
            // (broken box-drawing + UTF-8 input in TUIs).
            LANG: process.env.LANG || (process.platform === "darwin" ? "en_US.UTF-8" : "C.UTF-8"),
            LC_ALL: process.env.LC_ALL || (process.platform === "darwin" ? "en_US.UTF-8" : "C.UTF-8"),
            ...(isZsh ? { ZDOTDIR: ZSH_OVERRIDE_DIR } : {}),
            PROMPT_EOL_MARK: "",
            // Exists solely to trigger /etc/zshrc_Apple_Terminal; on Linux
            // it would just be a lie told to every TUI that sniffs it.
            ...(process.platform === "darwin" ? { TERM_PROGRAM: "Apple_Terminal" } : {}),
            ...(req.env ?? {}),
          },
        });
        const ch: Channel = { proc, exited: false, outRing: createSbRing(undefined, KEEPER_RING_CAP_BYTES), headSeq: 0 };
        channels.set(req.channel_id, ch);
        _log("info", "multiplexed-keeper", "child_spawned", {
          channelId: req.channel_id, pid: proc.pid, argv0: argv[0], cwd: req.cwd,
        });

        // Subprocess exit → emit Exit frame. proc.exited resolves with
        // the exit code (number) on normal exit, or null on signal.
        // `.catch` is mandatory — without it, any throw inside the
        // .then handler (encodeMuxFrame on an unexpected exit_code
        // shape, future Bun changes to proc.exited resolution type,
        // socket write failure during broadcast) becomes an unhandled
        // rejection that leaves ch.exited unset and channels Map dirty.
        proc.exited.then((exitCode) => {
          ch.exited = true;
          // DIAGNOSTIC for the "degraded keeper births dead PTYs" class
          // (CLAUDE.md L11 / feedback_claude_code_runs_inside_roost_keeper_pty):
          // a child that exits within ~2s having produced NO bytes
          // (headSeq===0) is a dead-birth — log WHY (exit_code 127=cmd not
          // found, signalCode=killed, exit 0/1 = startup script bombed).
          const lifetimeMs = Date.now() - spawnedAtMs;
          const deadBirth = lifetimeMs < 2000 && ch.headSeq === 0;
          _log(deadBirth ? "warn" : "info", "multiplexed-keeper", deadBirth ? "child_dead_birth" : "child_exited", {
            channelId: req.channel_id, pid: proc.pid, argv0: argv[0], cwd: req.cwd,
            exit_code: exitCode ?? null, signal: proc.signalCode ?? null,
            lifetime_ms: lifetimeMs, head_seq: ch.headSeq,
            ...(deadBirth ? { open_fds: _keeperOpenFdCount(), live_channels: channels.size } : {}),
          });
          broadcast(encodeMuxFrame(MuxFrameType.Exit, req.channel_id, JSON.stringify({ exit_code: exitCode ?? null })));
          channels.delete(req.channel_id);
          // Reclaim the PTY master fd — Bun does not auto-close it on child
          // exit, so without this each channel leaks one master fd (feeds the
          // FD-exhaustion diagnostics above). No-op if reap already closed it.
          try { proc.terminal?.close(); } catch { /* already closed */ }
        }).catch((err) => {
          _log("error", "multiplexed-keeper", "exit_handler_failed", { channelId: req.channel_id, error: String(err) });
          ch.exited = true;
          channels.delete(req.channel_id);
        });

        client.socket.write(encodeMuxFrame(MuxFrameType.SpawnAck, req.channel_id, JSON.stringify({ pid: proc.pid })));
      } catch (e) {
        // Bun.spawn threw synchronously (bad argv/cwd, PTY alloc failure,
        // EMFILE/ENOMEM). Previously sent to the client but NEVER logged
        // keeper-side → a spawn that fails before the child even starts was
        // invisible. Log it.
        _log("error", "multiplexed-keeper", "spawn_failed", {
          channelId: req.channel_id, argv0: argv[0], cwd: req.cwd, error: String(e),
          open_fds: _keeperOpenFdCount(), live_channels: channels.size,
        });
        client.socket.write(encodeMuxFrame(MuxFrameType.SpawnErr, req.channel_id, JSON.stringify({ error: String(e) })));
      }
      return;
    }
    case MuxFrameType.PtyIn: {
      const ch = channels.get(f.channelId);
      if (!ch || ch.exited) return;
      // Defensive copy. f.payload is a subarray view onto the keeper's
      // streaming receive buffer (protocol-v2.ts:75). Bun.spawn's
      // terminal.write does not document whether it consumes the
      // BufferSource argument synchronously; if it ever queues the
      // write for async flush, the receive buffer could roll under us
      // and the PTY would read whichever byte overwrote the slot.
      // node-pty's pty.write copied internally so this was invisible;
      // Bun gives us no guarantee. Cost is ~8 bytes per input frame,
      // immeasurable on the hot path. NOT the fix for the 2026-06-17
      // user-reported "backspace = space" symptom — that was env-level
      // (`TERM=unknown`), see the env: block above. Kept as defense.
      const safe = Buffer.from(f.payload);
      // Guard the non-null assertion explicitly. proc.terminal is only
      // populated when Bun.spawn was called with `terminal:` set (our
      // current spawn path always does). Future refactors that add a
      // non-terminal spawn branch would otherwise silently swallow
      // every keystroke for that session via the catch below — instead,
      // log so the misconfiguration is visible.
      const term = ch.proc.terminal;
      if (!term) {
        _log("error", "multiplexed-keeper", "ptyin_no_terminal", { channelId: f.channelId });
        return;
      }
      // A write that throws here means the PTY died in the race window before
      // proc.exited's .then set ch.exited — i.e. keystrokes vanishing into a
      // just-dead child ("can't type"). Was silently swallowed; log it.
      try { term.write(safe); } catch (e) {
        _log("warn", "multiplexed-keeper", "ptyin_write_failed", { channelId: f.channelId, error: String(e) });
      }
      return;
    }
    case MuxFrameType.Resize: {
      const ch = channels.get(f.channelId);
      // Match PtyIn's guard: also bail if proc exited but the channels
      // Map hasn't been cleaned yet (race between proc.exited resolving
      // and the .then microtask running channels.delete).
      if (!ch || ch.exited) return;
      try {
        const v = JSON.parse(f.payload.toString("utf8"));
        if (typeof v.cols === "number" && typeof v.rows === "number") {
          const term = ch.proc.terminal;
          if (!term) {
            // Symmetric with PtyIn — a non-terminal spawn branch would
            // silently drop resizes here too. Log so the misconfig is
            // diagnosable from a single grep instead of two.
            _log("error", "multiplexed-keeper", "resize_no_terminal", { channelId: f.channelId });
            return;
          }
          term.resize(v.cols, v.rows);
        }
      } catch { /* ignore malformed */ }
      return;
    }
    case MuxFrameType.KillChild: {
      const ch = channels.get(f.channelId);
      if (!ch || ch.exited) return;
      reapChannelTree(ch);
      return;
    }
    case MuxFrameType.Ping: {
      client.socket.write(encodeMuxFrame(MuxFrameType.Pong, 0, Buffer.alloc(0)));
      return;
    }
    case MuxFrameType.ListChannels: {
      const list: Array<{ channel_id: number; pid: number }> = [];
      for (const [channelId, ch] of channels.entries()) {
        if (!ch.exited) list.push({ channel_id: channelId, pid: ch.proc.pid });
      }
      client.socket.write(encodeMuxFrame(
        MuxFrameType.ListChannelsResp, 0,
        JSON.stringify({ channels: list }),
      ));
      return;
    }
    case MuxFrameType.Hello: {
      client.socket.write(encodeMuxFrame(
        MuxFrameType.HelloResp, 0,
        JSON.stringify({ version: KEEPER_PROTOCOL_VERSION, build: KEEPER_BUILD_STAMP }),
      ));
      return;
    }
    case MuxFrameType.GetHistory: {
      // RC2: reply with [8-byte BE head_seq][ring bytes] for the channel.
      // Unknown/exited channel → head_seq 0 + empty ring (worker treats it
      // as a fresh session, same as the pre-RC2 zeroing behavior).
      const ch = channels.get(f.channelId);
      const headSeq = ch ? ch.headSeq : 0;
      const ring = ch ? readRing(ch.outRing) : EMPTY_U8;
      const head = Buffer.allocUnsafe(8);
      head.writeBigUInt64BE(BigInt(headSeq), 0);
      client.socket.write(encodeMuxFrame(
        MuxFrameType.GetHistoryResp, f.channelId, Buffer.concat([head, ring]),
      ));
      return;
    }
    default: return;
  }
}

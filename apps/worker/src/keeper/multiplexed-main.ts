// Multiplexed keeper subprocess. One process per worker; hosts N PTYs
// over a single UDS. Channel_id discriminates per frame.
//
// Runs on BUN. Pre-2026-06-16 the keeper had to run on Node because
// node-pty's libuv polling didn't integrate with Bun's event loop —
// the worker (Bun) would spawn this subprocess under Node specifically
// for PTY access. Bun 1.3 ships native PTY via Bun.spawn({terminal:
// {...}}), so node-pty is gone and the runtime split with it.
//
// Entry for BOTH `bun run multiplexed-main.ts <sock>` (from source) and
// `roost keeper <sock>` (compiled binary self-exec via roost-cli/keeper.ts).
// The body lives in runKeeper() so importing this module for the subcommand
// has no side effects; only import.meta.main (source) auto-runs it.

import * as fs from "node:fs";
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { decodeMuxFrames } from "./protocol-v2.ts";
import { _log } from "./keeper-log.ts";
import { reapAllChannelsSync } from "./keeper-process-reap.ts";
import { handleFrame, type FrameHandlerCtx } from "./keeper-frame-handler.ts";
import type { Channel, ClientState } from "./keeper-types.ts";

export function runKeeper(sockPath: string): void {
  // Custom ZDOTDIR that prepends `setopt NO_PROMPT_SP NO_PROMPT_CR` to
  // the user's zsh startup. PROMPT_SP is zsh's "preserve partial line"
  // feature; it emits cols-1 spaces + CR before every prompt to push
  // any unterminated previous output to its own line. Without this, the
  // SPA's wterm shows whitespace junk between every command — the
  // PROMPT_EOL_MARK="" env var only hides the visible marker, not the
  // spaces. The custom .zshrc unsets the option then sources the user's
  // real ~/.zshrc so everything else (path, aliases, theme) still works.
  const ZSH_OVERRIDE_DIR = (() => {
    const dir = path.join(os.tmpdir(), "roost-zsh-noPROMPT_SP");
    try {
      fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
      fs.writeFileSync(path.join(dir, ".zshrc"), [
        "# roost: disable PROMPT_SP so the SPA wterm doesn't see whitespace junk",
        "unsetopt PROMPT_SP PROMPT_CR 2>/dev/null",
        "PROMPT_EOL_MARK=''",
        "# Source the real user zshrc so theme/aliases/path still load",
        'if [ -f "$HOME/.zshrc" ]; then source "$HOME/.zshrc"; fi',
        "# roost: emit OSC 7 (file://host/cwd) on every cd so the SPA",
        "# sidebar row label can update from session.cwd. Without this,",
        "# the sidebar shows the spawn cwd forever even after the user",
        "# `cd`s elsewhere. session-manager._scanOsc7 picks the sequence",
        "# up and emits a cwd SessionEvent.",
        "function roost_emit_osc7 { print -Pn \"\\e]7;file://${HOST}${PWD}\\e\\\\\" }",
        "autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook chpwd roost_emit_osc7",
        "roost_emit_osc7  # initial cwd on shell start",
        "",
      ].join("\n"));
    } catch (e) {
      _log("error", "multiplexed-keeper", "zdotdir_write_failed", { error: String(e) });
    }
    return dir;
  })();

  try { fs.unlinkSync(sockPath); } catch { /* ignore */ }
  const dir = path.dirname(sockPath);
  try { fs.mkdirSync(dir, { recursive: true, mode: 0o700 }); } catch { /* ignore */ }

  const channels = new Map<number, Channel>();
  const clients = new Set<ClientState>();

  function broadcast(frame: Buffer): void {
    for (const c of clients) {
      try { c.socket.write(frame); } catch { /* dead socket */ }
    }
  }

  // Single source of truth for keeper state, handed to the extracted frame
  // handler so `channels`/`broadcast`/`ZSH_OVERRIDE_DIR` live in exactly one
  // place (this entry) — the handler mutates them through the same references.
  const frameCtx: FrameHandlerCtx = { channels, broadcast, ZSH_OVERRIDE_DIR };

  const server = net.createServer((socket) => {
    const client: ClientState = { buf: Buffer.alloc(0), socket };
    clients.add(client);

    socket.on("data", (chunk: Buffer) => {
      client.buf = Buffer.concat([client.buf, chunk]);
      const { frames, remaining } = decodeMuxFrames(client.buf);
      client.buf = remaining;
      // Isolate each frame: a throw in one handler must NOT propagate out of
      // socket.on("data") → uncaught → whole-keeper crash that kills EVERY
      // session (incl. the live Claude one). Log + drop the bad frame instead.
      for (const f of frames) {
        try { handleFrame(frameCtx, client, f); }
        catch (e) { _log("error", "multiplexed-keeper", "handle_frame_failed", { type: f.type, channelId: f.channelId, error: String(e) }); }
      }
    });
    socket.on("close", () => { clients.delete(client); });
    socket.on("error", (e) => {
      _log("warn", "multiplexed-keeper", "client_socket_error", { error: String(e) });
      clients.delete(client);
    });
  });

  server.on("error", (e) => {
    // listen() failure (EADDRINUSE on a stale socket, permission) — was an
    // unhandled 'error' event → silent keeper crash at boot.
    _log("error", "multiplexed-keeper", "server_error", { error: String(e), sockPath });
  });

  server.listen(sockPath, () => {
    try { fs.chmodSync(sockPath, 0o600); } catch { /* ignore */ }
    _log("info", "multiplexed-keeper", "listening", { sockPath });
  });

  // The keeper outlives the worker and hosts the live Claude PTY — its own
  // silent crash is the worst failure (every session dies, cause lost). Log
  // the cause before the process goes. NOT swallowed: rethrow-free but we let
  // the runtime decide exit (unhandledRejection stays non-fatal; an uncaught
  // exception still terminates after we've recorded why).
  process.on("uncaughtException", (e) => {
    _log("error", "multiplexed-keeper", "uncaught_exception", { error: String(e), stack: e?.stack ?? null });
  });
  process.on("unhandledRejection", (reason) => {
    _log("error", "multiplexed-keeper", "unhandled_rejection", { reason: String(reason) });
  });

  // External SIGTERM (worker's stale-keeper kill at main.ts, restartKeeper on
  // deploy/kickstart). Default action = terminate with NO child cleanup → every
  // PTY child orphans to launchd. Sweep the trees first, then exit.
  process.on("SIGTERM", () => {
    _log("info", "multiplexed-keeper", "sigterm_reaping_children", { channels: channels.size });
    reapAllChannelsSync(channels);
    process.exit(0);
  });

  setInterval(() => {
    if (!fs.existsSync(sockPath)) {
      _log("info", "multiplexed-keeper", "socket_removed_exiting");
      reapAllChannelsSync(channels);
      process.exit(0);
    }
  }, 30_000);
}

if (import.meta.main) {
  const sock = process.argv[2];
  if (!sock) {
    _log("error", "multiplexed-keeper", "missing_socket_path_arg");
    process.exit(2);
  }
  runKeeper(sock);
}

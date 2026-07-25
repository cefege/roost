// Composer — bottom input + send, with two transport paths:
//
//  NATIVE (quick-chat sessions, cwd under ~/.roost/chats): the proper omp
//  integration — sessionsChatCommand tunnels {type:"prompt"} to the worker's
//  `omp --mode rpc` child for this session (lazy-started on first command).
//  No PTY, no TUI, no keystroke timing; the reply streams back as ChatFrames.
//
//  LEGACY (regular terminal sessions running the omp TUI): PTY injection —
//  buildPtyPayload + delayed CR, exactly like TerminalChatButton. The chat
//  overlay mirrors what the terminal does.
//
// Layout is a column: an attachment chip tray (only when something is pending)
// above the iOS-style row — a circular attach button on the left, the textarea
// + model/effort chip inside one rounded container, and a right-hand circle
// that is a mic while the composer is empty and Send once it isn't.

import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { rootStore } from "../../../store/root.ts";
import { ompChatForSession } from "../../../store/chatOmp.ts";
import { isChatFolder } from "../../../lib/quickChat.ts";
import { pickFilesTo } from "../../../lib/attachments.ts";
import { inputChannel } from "../../../ws/input-channel.ts";
import { buildBracketedPaste, buildPtyPayload, CR_BYTES, enterDelayMs } from "../../../lib/ptyPaste.ts";
import { Icon } from "../../Settings/md/Icon.tsx";
import { MobileVoiceInput } from "../../MobileVoiceInput.tsx";
import { isTouchDevice } from "../../../lib/windowSizeClass.ts";
import { ChatImage } from "./ChatImage.tsx";
import { ModelMenu } from "./ModelMenu.tsx";
import { ompCommand } from "./rpcCommand.ts";

/** Textarea growth ceiling — past this the draft scrolls instead of eating
 *  the transcript. Mirrors TerminalChatButton's composer. */
const MAX_INPUT_PX = 140;

/** One uploaded-but-not-yet-sent attachment. Owned by OmpChatPane (it owns the
 *  drop target); the composer owns the tray and the send encoding.
 *  `thumb` is a client-side data: URL for images — the browser already holds
 *  the bytes, so a 24px chip must not re-download the file from the worker. */
export interface Pending {
  absPath: string;
  name: string;
  mime: string;
  isImage: boolean;
  thumb?: string;
}

interface Props {
  sessionId: string;
  /** True only for the pane the user is on. The deck keeps EVERY open session
   *  mounted, so an unconditional focus here would let the last-mounted chat
   *  pane steal the keyboard from whatever the user is actually using. */
  focused?: boolean;
  pending: () => Pending[];
  addPending: (absPath: string, file: File) => void;
  removePending: (absPath: string) => void;
  clearPending: () => void;
}

export function Composer(props: Props) {
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  let taEl: HTMLTextAreaElement | undefined;

  // The composer takes the keyboard once the chat overlay owns a FOCUSED pane:
  // CellTerminal's focus gate hands it over (chatViewActive), and a pasted
  // image only reaches OmpChatPane's element-scoped handler if focus is inside
  // the pane. Touch skips it so switching to chat doesn't pop the keyboard.
  createEffect(() => { if (props.focused && !isTouchDevice()) taEl?.focus(); });

  const session = () => rootStore.sessions[props.sessionId];
  const isNative = () => isChatFolder(session()?.cwd ?? "");
  // Worker-owned turn state. Native engine: the rpc child's own flag. Mirror
  // engine: derived from omp's OSC title separator (Braille = working), so a
  // PTY-injection session now lights the badge and Stop too.
  // createMemo, not a bare accessor: the slot may not exist on first render,
  // and a plain read of the fallback literal registers no store dependency.
  const chat = createMemo(() => ompChatForSession(props.sessionId));

  const sendNative = async (body: string) => {
    try {
      await ompCommand(props.sessionId, { type: "prompt", message: body }, "Chat send");
    } finally {
      setSending(false);
    }
  };

  /** Size the box to its content, capped. Every draft mutation runs through
   *  here — typing, an inserted attachment path, a dictated transcript — so a
   *  programmatic insert never leaves long text scrolling in a one-row field. */
  const fitHeight = () => {
    if (!taEl) return;
    taEl.style.height = "auto";
    taEl.style.height = `${Math.min(taEl.scrollHeight, MAX_INPUT_PX)}px`;
  };

  const onInput = (e: InputEvent & { currentTarget: HTMLTextAreaElement }) => {
    setText(e.currentTarget.value);
    fitHeight();
  };

  /** Append with one separating space. queueMicrotask: the DOM value lands
   *  when Solid flushes the `value` binding, and scrollHeight is read from it. */
  const appendDraft = (s: string) => {
    setText((cur) => (cur ? `${cur} ${s}` : s));
    queueMicrotask(fitHeight);
  };

  const attach = () => {
    const s = session();
    if (!s) return;
    pickFilesTo(s, props.addPending);
  };

  const send = () => {
    const draft = text().trim();
    const atts = props.pending();
    // An attachment-only message is legitimate — the files ARE the payload.
    if ((!draft && atts.length === 0) || sending()) return;
    setSending(true);
    setText("");
    props.clearPending();
    // Collapse the box back to one row: a sent long draft otherwise leaves a
    // tall empty field behind.
    queueMicrotask(fitHeight);
    if (isNative()) {
      // `@"path"` mentions: omp runs extractFileMentions inside
      // AgentSession.prompt(), which the RPC prompt handler calls too — so the
      // file is read SERVER-side (text inlined as <file path=…>, images
      // attached as ImageContent). Always the quoted form: attachment names
      // routinely carry spaces (macOS screenshots). The bytes are already on
      // the worker's disk beside omp; base64ing them back up the wire would
      // re-send a file we just uploaded.
      const mentions = atts.map((p) => `@"${p.absPath}"`).join(" ");
      void sendNative(mentions ? (draft ? `${mentions}\n${draft}` : mentions) : draft);
      return;
    }
    // Legacy TUI path. Attachments first, ONE bracketed paste each: omp's
    // extractBracketedImagePastePaths auto-attaches a paste only when EVERY
    // segment is an image path, so a mixed png+pdf selection in one paste
    // would attach neither. No `@` prefix here — that drives the editor's
    // completion popup, which would swallow the submitting CR.
    for (const p of atts) inputChannel.sendInput(props.sessionId, buildBracketedPaste(p.absPath));
    if (draft) inputChannel.sendInput(props.sessionId, buildPtyPayload(draft));
    // Delay budgeted over everything just written, not the draft alone.
    const delay = enterDelayMs(draft + atts.map((p) => p.absPath).join(""));
    setTimeout(() => {
      inputChannel.sendInput(props.sessionId, CR_BYTES);
      setSending(false);
    }, delay);
  };

  const onKey = (e: KeyboardEvent) => {
    // An IME candidate commit fires Enter too; sending there ships a
    // half-composed CJK message.
    if (e.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    // data-streaming mirrors the worker's turn flag: the smoke harness
    // (roost-smoke/run.js:418) samples it to prove a reply actually streamed.
    <div class="omp-composer" data-testid="omp-chat-composer" data-streaming={String(chat().streaming)}>
      {/* Uploaded-but-unsent attachments. They are NOT text in the draft: the
          send path encodes them per transport (@"path" mention on native, one
          bracketed paste each on the TUI). */}
      <Show when={props.pending().length > 0}>
        <div class="omp-composer__tray" data-testid="omp-chat-attachments">
          <For each={props.pending()}>
            {(p) => (
              <div class="omp-attach-chip" data-testid="omp-chat-attach-chip" title={p.absPath}>
                {/* thumb (a client-side data: URL) short-circuits ChatImage's
                    worker fetch — same component, no re-download. */}
                <Show when={p.isImage} fallback={<Icon name="description" size="sm" />}>
                  <ChatImage sessionId={props.sessionId} blobPath={p.thumb ?? p.absPath} mime={p.mime} />
                </Show>
                <span class="omp-attach-chip__name">{p.name}</span>
                <button
                  type="button"
                  class="omp-attach-chip__x"
                  aria-label={`Remove ${p.name}`}
                  onClick={() => props.removePending(p.absPath)}
                >
                  <Icon name="close" size="sm" />
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>

      <div class="omp-composer__row">
        <button
          type="button"
          class="omp-composer__round"
          data-testid="omp-chat-attach"
          aria-label="Attach file"
          disabled={!session()}
          onClick={attach}
        >
          <Icon name="add" />
        </button>

        <div class="omp-composer__pill">
          <textarea
            ref={taEl}
            class="omp-composer__input"
            rows={1}
            data-testid="omp-chat-input"
            placeholder="Message"
            aria-label="Message"
            disabled={sending()}
            value={text()}
            onInput={onInput}
            onKeyDown={onKey}
          />
          {/* Native engine only: the mirror engine reports no model, and a chip
              that cannot change anything would be a lie. */}
          <Show when={chat().model}>
            <ModelMenu
              sessionId={props.sessionId}
              model={chat().model}
              modelName={chat().modelName}
              thinkingLevel={chat().thinkingLevel}
            />
          </Show>
        </div>

        {/* Stop sits BESIDE Send, not in place of it: omp accepts a mid-turn
            prompt (the worker queues it as a followUp), so hiding Send would
            wrongly imply follow-ups are blocked. */}
        <Show when={chat().streaming}>
          <button
            type="button"
            class="omp-composer__round omp-composer__round--stop"
            data-testid="omp-chat-stop"
            aria-label="Stop"
            onClick={() => void ompCommand(props.sessionId, { type: "abort" }, "Stop")}
          >
            <Icon name="stop" />
          </button>
        </Show>

        <Show
          // Attachments alone are a sendable message, so they arm Send too.
          when={text().trim().length > 0 || props.pending().length > 0}
          fallback={
            // Empty draft → mic. Dictation lands in the DRAFT (onTranscript), not
            // the PTY, so the user can edit before sending. The wrapper is
            // position:relative so the inline caption anchors above the row.
            <Show when={session()}>
              {(s) => (
                <div class="omp-composer__mic">
                  <MobileVoiceInput
                    channelId={s().channel}
                    variant="inline"
                    sendInput={() => { /* unused: onTranscript owns the result */ }}
                    onTranscript={appendDraft}
                  />
                </div>
              )}
            </Show>
          }
        >
          <button
            type="button"
            class="omp-composer__round omp-composer__round--send"
            data-testid="omp-chat-send"
            aria-label="Send"
            disabled={sending()}
            onClick={() => send()}
          >
            <Icon name={sending() ? "more_horiz" : "arrow_upward"} />
          </button>
        </Show>
      </div>
    </div>
  );
}

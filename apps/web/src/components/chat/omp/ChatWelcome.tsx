// ChatWelcome — the empty chat pane's first screen. omp's own TUI splash is
// structurally unreachable here: Roost spawns the child with `--mode rpc-ui`,
// and omp's `isInteractive` is false for ANY --mode, so WelcomeComponent is
// never constructed and no RPC returns its text. This is the Roost-native
// equivalent — greeting + folder always, and a short tip list whose command
// names and descriptions are omp's real ones, pulled over the same tunnel the
// model picker uses.

import { createEffect, createSignal, For, Show } from "solid-js";
import { rootStore } from "../../../store/root.ts";
import { isChatFolder } from "../../../lib/quickChat.ts";
import { ompCommand } from "./rpcCommand.ts";
import { pickTips, type Tip } from "./welcomeTips.ts";

interface Props {
  sessionId: string;
  /** True only for the pane the user is on. Gates the tip fetch — the deck
   *  keeps every open session's pane mounted, and an unguarded fetch would
   *  spawn an omp child per backgrounded pane. */
  focused?: boolean;
}

export function ChatWelcome(props: Props) {
  const [tips, setTips] = createSignal<Tip[]>([]);
  // One fetch per pane, ever. `focused` can flip after mount (the deck mounts
  // background panes first), so this is an effect with a latch rather than an
  // onMount.
  let fetched = false;

  const folder = () => {
    const cwd = rootStore.sessions[props.sessionId]?.cwd ?? "";
    return cwd.split("/").filter(Boolean).pop() ?? "";
  };

  const load = async () => {
    // quiet: the user never asked for this list. A dead child drops the tips
    // block, it does not raise a toast.
    const data = await ompCommand(props.sessionId, { type: "get_available_commands" }, "Command list", true);
    if (data === null) return;
    setTips(pickTips(data));
  };

  createEffect(() => {
    // Mirror-engine sessions (terminal omp behind an OSC title) have no RPC
    // child; asking for commands would spawn one purely to draw tips. They get
    // greeting + folder only.
    if (fetched || !props.focused || !isChatFolder(rootStore.sessions[props.sessionId]?.cwd ?? "")) return;
    fetched = true;
    void load();
  });

  return (
    <div class="omp-chat__welcome" data-testid="omp-chat-welcome">
      <div class="omp-chat__welcome-title">Welcome back</div>
      <Show when={folder()}>
        <div class="omp-chat__welcome-sub">{folder()}</div>
      </Show>
      <Show when={tips().length > 0}>
        <div class="omp-chat__welcome-tips">
          <div class="omp-chat__welcome-sub">Tips</div>
          <For each={tips()}>
            {(tip) => (
              <div class="omp-chat__welcome-tip">
                <span class="omp-chat__welcome-cmd">/{tip.name}</span>
                <span>{tip.description}</span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

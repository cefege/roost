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
    // Latched: the pane only mounts for a `kind:"agent"` session, whose RPC
    // child exists from spawn, so the first focused render can ask for the
    // command catalog outright. (This used to be gated on the cwd, because a
    // mirror-engine session had no child to ask.)
    if (fetched || !props.focused) return;
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

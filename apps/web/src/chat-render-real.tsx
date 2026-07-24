// Real-session render check — mounts the REAL OmpChatPane over a full omp
// transcript (378 messages, all block kinds) parsed by the worker's parseOmpLine
// and dumped to chat-render-real.json. This is the parity eyeball: what the chat
// overlay actually paints for a real session. Served at /chat-render-real.html.

import { render } from "solid-js/web";
import { setRootStore } from "./store/root.ts";
import { setOmpChatView } from "./store/uiStore.ts";
import { OmpChatPane } from "./components/chat/omp/OmpChatPane.tsx";
import type { ChatMessage } from "@roost/shared/chat/wire";
import msgs from "./chat-render-real.json";

const SID = "00000000-0000-0000-0000-000000000001";
setRootStore("terminal_title", SID, "π: real session");
setOmpChatView(SID, "chat");
setRootStore("chat_omp", SID, { messages: msgs as ChatMessage[], seq: (msgs as ChatMessage[]).length, status: "resolved", streaming: false, model: "", contextPct: 0, contextTokens: 0 });

render(() => <OmpChatPane sessionId={SID} />, document.getElementById("app")!);

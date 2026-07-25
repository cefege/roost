// UI-only state: which modals are open, current context-menu target, etc.
// Separate from rootStore so transient UI state doesn't churn the wire-
// shaped data the SSE projector folds.
// sidebarOpen: mobile drawer state; on desktop the sidebar is always visible.

import { createStore } from "solid-js/store";
import { ompChatEnabled } from "./chatOmp.ts";

interface UIState {
  contextMenu: { x: number; y: number; sessionId: string } | null;
  renamingSessionId: string | null;
  sidebarOpen: boolean;               // mobile: drawer open; desktop: ignored
  sidebarCollapsed: boolean;          // desktop: collapses to icon rail (⌘B)
  sidebarWidth: number;               // desktop: pixel width — drag-resizable
  homeFolderViewMode: "grid" | "list"; // home page: grid vs dense list of folders
  homeFolderShowFiles: boolean;        // home/browse: reveal view-only files alongside folders
  notificationBellOpen: boolean;       // notification bell dropdown open (shared with mobile bars)
  bellAnchorEl: HTMLElement | null;    // anchor element for notification dropdown positioning
  /** Per-session chat view mode (omp only). "terminal" = cell grid;
   *  "chat" = OmpChatPane. Persisted to localStorage per session id. omp
   *  sessions default to "chat"; everything else to "terminal". A future
   *  Claude chat adds its own branch, not a value here. */
  chatViewBySession: Record<string, "terminal" | "chat">;
}

const CHAT_VIEW_KEY = "roost.chatViewBySession";
function loadChatView(): Record<string, "terminal" | "chat"> {
  try {
    const raw = localStorage.getItem(CHAT_VIEW_KEY);
    const obj = raw ? JSON.parse(raw) : null;
    return obj && typeof obj === "object" ? obj as Record<string, "terminal" | "chat"> : {};
  } catch { return {}; }
}
function persistChatView(map: Record<string, "terminal" | "chat">): void {
  try { localStorage.setItem(CHAT_VIEW_KEY, JSON.stringify(map)); } catch { /* quota */ }
}

const SIDEBAR_COLLAPSED_KEY = "roost.sidebarCollapsed";
const SIDEBAR_WIDTH_KEY = "roost.sidebarWidth";
const SIDEBAR_WIDTH_DEFAULT = 300;
const SIDEBAR_WIDTH_MIN = 200;
const SIDEBAR_WIDTH_MAX = 600;
const HOME_FOLDER_VIEW_MODE_KEY = "roost.homeFolderViewMode";
const HOME_FOLDER_SHOW_FILES_KEY = "roost.homeFolderShowFiles";
function loadCollapsed(): boolean {
  try { return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "1"; } catch { return false; }
}
function persistCollapsed(v: boolean) {
  try { localStorage.setItem(SIDEBAR_COLLAPSED_KEY, v ? "1" : "0"); } catch { /* ignore */ }
}
function loadHomeFolderViewMode(): "grid" | "list" {
  try {
    const v = localStorage.getItem(HOME_FOLDER_VIEW_MODE_KEY);
    return v === "grid" ? "grid" : "list";
  } catch { return "list"; }
}
function persistHomeFolderViewMode(v: "grid" | "list") {
  try { localStorage.setItem(HOME_FOLDER_VIEW_MODE_KEY, v); } catch { /* ignore */ }
}
function loadHomeFolderShowFiles(): boolean {
  try { return localStorage.getItem(HOME_FOLDER_SHOW_FILES_KEY) === "1"; } catch { return false; }
}
function persistHomeFolderShowFiles(v: boolean) {
  try { localStorage.setItem(HOME_FOLDER_SHOW_FILES_KEY, v ? "1" : "0"); } catch { /* ignore */ }
}
function loadWidth(): number {
  try {
    const raw = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    if (!raw) return SIDEBAR_WIDTH_DEFAULT;
    const n = parseInt(raw, 10);
    if (!Number.isFinite(n)) return SIDEBAR_WIDTH_DEFAULT;
    return Math.max(SIDEBAR_WIDTH_MIN, Math.min(SIDEBAR_WIDTH_MAX, n));
  } catch { return SIDEBAR_WIDTH_DEFAULT; }
}
function persistWidth(v: number) {
  try { localStorage.setItem(SIDEBAR_WIDTH_KEY, String(v)); } catch { /* ignore */ }
}

export const [uiStore, setUiStore] = createStore<UIState>({
  contextMenu: null,
  renamingSessionId: null,
  sidebarOpen: false,
  sidebarCollapsed: loadCollapsed(),
  sidebarWidth: loadWidth(),
  homeFolderViewMode: loadHomeFolderViewMode(),
  homeFolderShowFiles: loadHomeFolderShowFiles(),
  notificationBellOpen: false,
  bellAnchorEl: null,
  chatViewBySession: loadChatView(),
});

export const setSidebarWidth = (px: number) => {
  const clamped = Math.max(SIDEBAR_WIDTH_MIN, Math.min(SIDEBAR_WIDTH_MAX, Math.round(px)));
  setUiStore("sidebarWidth", clamped);
  persistWidth(clamped);
};

export const openContextMenu = (x: number, y: number, sessionId: string) =>
  setUiStore("contextMenu", { x, y, sessionId });
export const dismissContextMenu = () => setUiStore("contextMenu", null);
export const startRename = (sessionId: string) => setUiStore("renamingSessionId", sessionId);
export const stopRename = () => setUiStore("renamingSessionId", null);
export const openSidebar = () => setUiStore("sidebarOpen", true);
export const closeSidebar = () => setUiStore("sidebarOpen", false);
export const toggleSidebar = () => setUiStore("sidebarOpen", (v) => !v);
export const openNotificationBell = () => setUiStore("notificationBellOpen", true);
export const closeNotificationBell = () => setUiStore("notificationBellOpen", false);
export const toggleNotificationBell = () => setUiStore("notificationBellOpen", (v) => !v);
export const setBellAnchor = (el: HTMLElement | null) => setUiStore("bellAnchorEl", el);
export const toggleSidebarCollapsed = () => {
  setUiStore("sidebarCollapsed", (v) => {
    const next = !v;
    persistCollapsed(next);
    return next;
  });
};
export const setHomeFolderViewMode = (mode: "grid" | "list") => {
  setUiStore("homeFolderViewMode", mode);
  persistHomeFolderViewMode(mode);
};
export const setHomeFolderShowFiles = (v: boolean) => {
  setUiStore("homeFolderShowFiles", v);
  persistHomeFolderShowFiles(v);
};
/** Per-session chat view mode. An omp session defaults to "chat" — the chat IS
 *  the session's native surface, and landing the user on a cell grid hides it
 *  behind a corner toggle. Everything else defaults to "terminal". An explicit
 *  toggle is persisted per session and always wins, so the terminal is one
 *  click away. */
export function ompChatViewForSession(sessionId: string): "terminal" | "chat" {
	const explicit = uiStore.chatViewBySession[sessionId];
	if (explicit) return explicit;
	return ompChatEnabled(sessionId) ? "chat" : "terminal";
}
export function setOmpChatView(sessionId: string, mode: "terminal" | "chat"): void {
  setUiStore("chatViewBySession", sessionId, mode);
  persistChatView(uiStore.chatViewBySession);
}
export function toggleOmpChatView(sessionId: string): void {
  setOmpChatView(sessionId, ompChatViewForSession(sessionId) === "chat" ? "terminal" : "chat");
}

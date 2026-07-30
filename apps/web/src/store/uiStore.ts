// UI-only state: which modals are open, current context-menu target, etc.
// Separate from rootStore so transient UI state doesn't churn the wire-
// shaped data the SSE projector folds.
// sidebarOpen: mobile drawer state; on desktop the sidebar is always visible.

import { createStore } from "solid-js/store";

interface UIState {
  sidebarOpen: boolean;               // mobile: drawer open; desktop: ignored
  sidebarCollapsed: boolean;          // desktop: collapses to icon rail (⌘B)
  sidebarWidth: number;               // desktop: pixel width — drag-resizable
  homeFolderViewMode: "grid" | "list"; // home page: grid vs dense list of folders
  homeFolderShowFiles: boolean;        // home/browse: reveal view-only files alongside folders
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
  sidebarOpen: false,
  sidebarCollapsed: loadCollapsed(),
  sidebarWidth: loadWidth(),
  homeFolderViewMode: loadHomeFolderViewMode(),
  homeFolderShowFiles: loadHomeFolderShowFiles(),
});

export const setSidebarWidth = (px: number) => {
  const clamped = Math.max(SIDEBAR_WIDTH_MIN, Math.min(SIDEBAR_WIDTH_MAX, Math.round(px)));
  setUiStore("sidebarWidth", clamped);
  persistWidth(clamped);
};

export const openSidebar = () => setUiStore("sidebarOpen", true);
export const closeSidebar = () => setUiStore("sidebarOpen", false);
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

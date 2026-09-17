// Bottom inset the notification dock must clear, as a CSS length expression.
// Lives apart from NotificationDock.tsx so the three bottom-chrome cases are
// decidable (and testable) without a DOM or a JSX runtime.
// Inputs come from TerminalComposeButton's composer signals and windowSizeClass.

export interface NotificationDockChrome {
  composerActive: boolean;
  composerHeightPx: number;
  compact: boolean;
}

export function notificationDockLift(chrome: NotificationDockChrome): string {
  // The viewport composer is the only bottom chrome whose height moves, and its
  // own offset already folds the safe area and the soft-keyboard inset.
  if (chrome.composerActive) {
    return `calc(var(--term-chat-dock-offset) + ${chrome.composerHeightPx}px + var(--md-space-2))`;
  }
  if (chrome.compact) return "var(--term-chat-dock-offset)";
  // Pointer layouts: clear the status bar and the in-pane composer's resting row
  // with one constant, so the dock never depends on route or pane count.
  return "calc(var(--workbench-statusbar-height) + var(--term-chat-rest-height) + max(var(--kb-offset), 0px) + var(--md-space-4))";
}

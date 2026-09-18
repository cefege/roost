// NotificationDock — the single bottom-anchored overlay column. Owns where every
// transient notification sits (toasts, undo snackbars, transfers, pair requests)
// so no two can claim the same corner, and rides above whichever bottom chrome is
// mounted: the compact viewport composer plus soft keyboard, or the desktop status
// bar plus the in-pane composer row (lib/notificationDockLift.ts decides which).
// Mounted once by App.tsx's RootShell; each child owns its own state module.

import { ToastStack } from "./ToastStack.tsx";
import { UndoCloseBanner } from "./UndoCloseBanner.tsx";
import { TransferStack } from "./TransferCard.tsx";
import { PairRequestNotifier } from "./PairRequestNotifier.tsx";
import { PadHintBar } from "./PadHintBar.tsx";
import { composerActive, composerHeightPx } from "./TerminalComposeButton.tsx";
import { notificationDockLift } from "../lib/notificationDockLift.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import "./NotificationDock.css";

export function NotificationDock() {
  return (
    <div
      class="roost-notify-dock"
      data-testid="notification-dock"
      style={{
        "--roost-notify-dock-lift": notificationDockLift({
          composerActive: composerActive(),
          composerHeightPx: composerHeightPx(),
          compact: isCompact(),
        }),
      }}
    >
      <ToastStack />
      <UndoCloseBanner />
      <TransferStack />
      <PairRequestNotifier />
      <PadHintBar />
    </div>
  );
}

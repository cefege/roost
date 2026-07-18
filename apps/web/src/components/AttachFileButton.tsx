// AttachFileButton — M3 FAB, always shown in the terminal. Tap → native file
// picker → each chosen file uploads (chunked, with a progress chip) and its
// abs_path is injected into the PTY, exactly like a drag-drop. Same backend as
// the drop handler and the right-click "Attach file" item (attachments.ts::
// pickAndAttachFiles). Modeled on AgentLaunchButton; sits left of the
// agent-launch / mic FABs in the bottom-right cluster.

import type { Component } from "solid-js";
import { pickAndAttachFiles } from "../lib/attachments.ts";
import { onFabPointerDown } from "../lib/fabDragOffset.ts";
import type { Session } from "@roost/shared/wire";

export const AttachFileButton: Component<{ session: Session }> = (props) => (
  <button
    type="button"
    class="attach-file-fab"
    data-testid="attach-file"
    aria-label="Attach a file — uploads it and inserts its path into this terminal"
    title="Attach file"
    // MOUSEDOWN preventDefault: keep terminal focus so injected path doesn't
    // land in a blurred pane (feedback_controls_near_terminal_must_not_steal_focus).
    onMouseDown={(e) => e.preventDefault()}
    onPointerDown={onFabPointerDown}
    onClick={() => pickAndAttachFiles(props.session)}
  >
    <span class="attach-file-fab__icon">attach_file</span>
  </button>
);

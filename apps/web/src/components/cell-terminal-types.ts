// Defines the reactive inputs accepted by the canonical terminal pane.
// CellTerminal and its controller leaves share this single props contract.
// Imperative pane resources live separately in cell-terminal-runtime.ts.

import type { Session } from "@roost/shared/wire";

export interface CellTerminalProps {
	session: Session;
	// In the current tiling layout (a visible pane's selected tab) → publish view
	// membership and render cells. Parked tabs remain mounted but inactive.
	inLayout?: boolean;
	// Owns the keyboard = the focused pane's selected tab. Only the focused pane
	// force-focuses and runs the document focus-recovery nets.
	focused?: boolean;
	// The floated pane's selected tab. Flipping this forces an exact re-fit.
	spotlit?: boolean;
	// False while a non-terminal route overlays the persistent deck.
	surfaceVisible: boolean;
	// False when another desktop pane's spotlight scrim covers this surface.
	surfaceActive: boolean;
}

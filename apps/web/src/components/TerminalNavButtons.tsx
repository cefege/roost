// TerminalNavButtons — the touch terminal-key sheet. It routes navigation
// through the hidden wterm encoder so cursor/application modes stay correct.
// Sheet visibility is shared and persisted independently of the composer.

import { createSignal, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { mouseForwardEnabled, toggleMouseForward } from "../lib/mouseForwardPref.ts";

interface Props {
	onKey: (key: string) => void;
	ctrlArmed: boolean;
	onCtrlArmedChange: (armed: boolean) => void;
}

const PAD_OPEN_KEY = "roostNavPadOpen";

const readPadOpen = (): boolean => {
	try {
		return localStorage.getItem(PAD_OPEN_KEY) === "1";
	} catch {
		return false;
	}
};

const [navPadOpen, setNavPadOpen] = createSignal(readPadOpen());

const persistNavPadOpen = (open: boolean): void => {
	setNavPadOpen(open);
	try {
		localStorage.setItem(PAD_OPEN_KEY, open ? "1" : "0");
	} catch {
		/* ignore unavailable storage */
	}
};

export function TerminalNavButtons(props: Props) {
	const togglePad = () => {
		const next = !navPadOpen();
		if (!next) props.onCtrlArmedChange(false);
		persistNavPadOpen(next);
	};

	return (
		<Portal>
			<Show when={navPadOpen()}>
				<div class="term-nav" data-testid="terminal-nav-buttons">
					<div class="term-nav__grid">
						<NavKey area="esc" testid="nav-esc" label="esc" ariaLabel="Escape" onClick={() => props.onKey("Escape")} />
						<NavKey area="tab" testid="nav-tab" label="tab" ariaLabel="Tab" onClick={() => props.onKey("Tab")} />
						<button
							type="button"
							class="term-nav__key term-nav__key--ctrl"
							data-testid="nav-ctrl"
							data-active={props.ctrlArmed ? "true" : "false"}
							aria-label="Control"
							aria-pressed={props.ctrlArmed}
							onMouseDown={(e) => e.preventDefault()}
							onClick={() => props.onCtrlArmedChange(!props.ctrlArmed)}
						>
							<span class="term-nav__label">ctrl</span>
						</button>
						<NavKey area="back" testid="nav-backspace" icon="backspace" ariaLabel="Backspace" onClick={() => props.onKey("Backspace")} />
						<NavKey area="home" testid="nav-home" label="home" ariaLabel="Home" onClick={() => props.onKey("Home")} />
						<NavKey area="up" testid="nav-up" icon="keyboard_arrow_up" ariaLabel="Up arrow" onClick={() => props.onKey("ArrowUp")} />
						<NavKey area="end" testid="nav-end" label="end" ariaLabel="End" onClick={() => props.onKey("End")} />
						<NavKey area="pgup" testid="nav-pgup" icon="keyboard_double_arrow_up" ariaLabel="Page up" onClick={() => props.onKey("PageUp")} />
						<NavKey area="left" testid="nav-left" icon="keyboard_arrow_left" ariaLabel="Left arrow" onClick={() => props.onKey("ArrowLeft")} />
						<NavKey area="down" testid="nav-down" icon="keyboard_arrow_down" ariaLabel="Down arrow" onClick={() => props.onKey("ArrowDown")} />
						<NavKey area="right" testid="nav-right" icon="keyboard_arrow_right" ariaLabel="Right arrow" onClick={() => props.onKey("ArrowRight")} />
						<NavKey area="pgdn" testid="nav-pgdn" icon="keyboard_double_arrow_down" ariaLabel="Page down" onClick={() => props.onKey("PageDown")} />
						<NavKey area="enter" testid="nav-enter" icon="keyboard_return" ariaLabel="Enter" onClick={() => props.onKey("Enter")} />
						<button
							type="button"
							class="term-nav__key term-nav__key--mouse"
							data-testid="nav-mouse"
							data-active={mouseForwardEnabled() ? "true" : "false"}
							aria-label="Toggle mouse forwarding"
							aria-pressed={mouseForwardEnabled()}
							onMouseDown={(e) => e.preventDefault()}
							onClick={toggleMouseForward}
						>
							<span class="term-nav__icon">mouse</span>
							<span class="term-nav__label">{mouseForwardEnabled() ? "on" : "off"}</span>
						</button>
					</div>
				</div>
			</Show>
			<button
				type="button"
				class="term-nav-toggle"
				data-testid="terminal-nav-toggle"
				data-open={navPadOpen() ? "true" : "false"}
				aria-label={navPadOpen() ? "Hide terminal keys" : "Show terminal keys"}
				onMouseDown={(e) => e.preventDefault()}
				onClick={togglePad}
			>
				<span class="term-nav-toggle__icon">
					{navPadOpen() ? "keyboard_arrow_down" : "keyboard"}
				</span>
			</button>
		</Portal>
	);
}

function NavKey(props: {
	area: "esc" | "tab" | "ctrl" | "back" | "home" | "up" | "end" | "pgup" | "left" | "down" | "right" | "pgdn" | "enter";
	testid: string;
	icon?: string;
	label?: string;
	ariaLabel: string;
	onClick: () => void;
}) {
	return (
		<button
			type="button"
			class={`term-nav__key term-nav__key--${props.area}`}
			data-testid={props.testid}
			aria-label={props.ariaLabel}
			onMouseDown={(e) => e.preventDefault()}
			onClick={props.onClick}
		>
			{props.icon
				? <span class="term-nav__icon">{props.icon}</span>
				: <span class="term-nav__label">{props.label}</span>}
		</button>
	);
}

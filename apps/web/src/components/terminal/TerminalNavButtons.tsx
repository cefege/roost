// TerminalNavButtons — the touch terminal-key sheet. It routes navigation
// through the hidden wterm encoder so cursor/application modes stay correct.
// Sheet visibility is shared and persisted independently of the composer.
// Its Alt control is a local link-activation latch, never terminal input.

import { onCleanup, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { mouseForwardEnabled, toggleMouseForward } from "../../store/prefs/mouseForwardPref.ts";
import {
	registerTerminalNavPadDisarm,
	terminalNavPadOpen,
	toggleTerminalNavPad,
} from "../../store/terminalNavPad.ts";
import { Button, Icon, IconButton } from "../Settings/md/primitives.tsx";

interface Props {
	onKey: (key: string) => void;
	ctrlArmed: boolean;
	onCtrlArmedChange: (armed: boolean) => void;
	linkActivationArmed: boolean;
	onLinkActivationArmedChange: (armed: boolean) => void;
}


export function TerminalNavButtons(props: Props) {
	const disarmThisSheet = () => {
		props.onCtrlArmedChange(false);
		props.onLinkActivationArmedChange(false);
	};
	onCleanup(registerTerminalNavPadDisarm(disarmThisSheet));

	return (
		<Portal>
			<Show when={terminalNavPadOpen()}>
				<div class="term-nav" data-testid="terminal-nav-buttons">
					<div class="term-nav__grid">
						<NavKey area="esc" testid="nav-esc" label="esc" ariaLabel="Escape" onClick={() => props.onKey("Escape")} />
						<NavKey area="tab" testid="nav-tab" label="tab" ariaLabel="Tab" onClick={() => props.onKey("Tab")} />
						<Button
							type="button"
							variant="secondary"
							size="icon"
							class="term-nav__key term-nav__key--ctrl"
							data-testid="nav-ctrl"
							data-active={props.ctrlArmed ? "true" : "false"}
							aria-label="Control"
							aria-pressed={props.ctrlArmed}
							onMouseDown={(e) => e.preventDefault()}
							onClick={() => props.onCtrlArmedChange(!props.ctrlArmed)}
						>
							<span class="term-nav__label">ctrl</span>
						</Button>
						<Button
							type="button"
							variant="secondary"
							size="icon"
							class="term-nav__key term-nav__key--alt"
							data-testid="nav-alt"
							data-active={props.linkActivationArmed ? "true" : "false"}
							aria-label="Toggle Alt link activation"
							aria-pressed={props.linkActivationArmed}
							onMouseDown={(e) => e.preventDefault()}
							onClick={() => props.onLinkActivationArmedChange(!props.linkActivationArmed)}
						>
							<span class="term-nav__label">alt</span>
						</Button>
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
						<Button
							type="button"
							variant="secondary"
							size="icon"
							class="term-nav__key term-nav__key--mouse"
							data-testid="nav-mouse"
							data-active={mouseForwardEnabled() ? "true" : "false"}
							aria-label="Toggle mouse forwarding"
							aria-pressed={mouseForwardEnabled()}
							onMouseDown={(e) => e.preventDefault()}
							onClick={toggleMouseForward}
						>
							<Icon name="mouse" class="term-nav__icon" />
							<span class="term-nav__label">{mouseForwardEnabled() ? "on" : "off"}</span>
						</Button>
					</div>
				</div>
			</Show>
			<IconButton
				type="button"
				variant="ghost"
				size="icon-lg"
				class="term-nav-toggle"
				data-testid="terminal-nav-toggle"
				data-open={terminalNavPadOpen() ? "true" : "false"}
				label={terminalNavPadOpen() ? "Hide terminal keys" : "Show terminal keys"}
				onMouseDown={(e) => e.preventDefault()}
				onClick={toggleTerminalNavPad}
				icon={terminalNavPadOpen() ? "keyboard_arrow_down" : "keyboard"}
			/>
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
		<Button
			type="button"
			variant="secondary"
			size="icon"
			class={`term-nav__key term-nav__key--${props.area}`}
			data-testid={props.testid}
			aria-label={props.ariaLabel}
			onMouseDown={(e) => e.preventDefault()}
			onClick={props.onClick}
		>
			{props.icon
				? <Icon name={props.icon} class="term-nav__icon" />
				: <span class="term-nav__label">{props.label}</span>}
		</Button>
	);
}

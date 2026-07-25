// ModelMenu — the composer pill's trailing chip plus the floating picker it
// opens. Owns its trigger (like ArrangeMenu owns its button) so the anchor rect
// and the open signal never have to cross a component boundary.
//
// The chip is REAL state, not decoration: it renders the omp session's live
// model + thinking level off the ChatFrame, and picking a row issues
// set_model / set_thinking_level through the SessionsChatCommand tunnel. The
// worker's config_update handler pushes the new state back within a round
// trip, so nothing here mutates the store optimistically.

import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { ctxMenuSurfaceStyle, CtxMenuItem, CtxMenuSeparator } from "../../contextMenuPrimitives.tsx";
import { Icon } from "../../Settings/md/Icon.tsx";
import { ompCommand } from "./rpcCommand.ts";

interface Props {
	sessionId: string;
	/** "provider/id" — the selector omp reports in get_state. */
	model: string;
	modelName: string;
	thinkingLevel: string;
}

/** The subset of omp's `Model` this menu needs. */
interface ModelRow {
	provider: string;
	id: string;
	name: string;
	/** false → the model has no reasoning at all; the effort section is dead. */
	reasoning: boolean;
	/** The model's own ladder when it publishes one; empty → use the default. */
	efforts: string[];
}

/** omp's full ThinkingLevel ladder, used when a model publishes no `efforts`.
 *  `off` is appended separately so it is always the last row. */
const DEFAULT_EFFORTS = ["minimal", "low", "medium", "high", "xhigh", "max"];

/** Capitalise an omp enum value ("low" → "Low"). Shared with the pane's status
 *  row so the thinking-level suffix reads identically in both places. */
export const cap = (s: string) => (s ? s[0]!.toUpperCase() + s.slice(1) : s);

/** Decode the catalog the worker projected out of get_available_models
 *  (rpc-chat.ts::trimCatalog). Still guarded field by field: it crossed a JSON
 *  tunnel, and a malformed entry is dropped rather than rendered half-built. */
function parseModels(data: unknown): ModelRow[] {
	if (!data || typeof data !== "object" || !("models" in data)) return [];
	const raw = data.models;
	if (!Array.isArray(raw)) return [];
	const out: ModelRow[] = [];
	for (const m of raw) {
		if (!m || typeof m !== "object") continue;
		const provider = "provider" in m && typeof m.provider === "string" ? m.provider : "";
		const id = "id" in m && typeof m.id === "string" ? m.id : "";
		if (!provider || !id) continue;
		const name = "name" in m && typeof m.name === "string" && m.name ? m.name : id;
		const reasoning = !("reasoning" in m) || m.reasoning !== false;
		const rawEfforts = "efforts" in m ? m.efforts : null;
		const efforts = Array.isArray(rawEfforts) ? rawEfforts.filter((v): v is string => typeof v === "string") : [];
		out.push({ provider, id, name, reasoning, efforts });
	}
	out.sort((a, b) => a.provider.localeCompare(b.provider) || a.name.localeCompare(b.name));
	return out;
}

export function ModelMenu(props: Props) {
	// null = never opened / still loading. `failed` distinguishes "no catalog"
	// from "empty catalog" so the menu can say which.
	const [models, setModels] = createSignal<ModelRow[] | null>(null);
	const [failed, setFailed] = createSignal(false);
	const [anchor, setAnchor] = createSignal<DOMRect | null>(null);
	// A real omp catalog is ~590 models; an unfiltered list is a scroll marathon.
	const [query, setQuery] = createSignal("");
	let btnEl: HTMLButtonElement | undefined;
	let menuEl: HTMLDivElement | undefined;

	const label = () => {
		const head = props.modelName || props.model;
		const lvl = props.thinkingLevel;
		return lvl && lvl !== "off" ? `${head} · ${cap(lvl)}` : head;
	};

	const toggle = () => {
		if (anchor()) {
			setAnchor(null);
			return;
		}
		setAnchor(btnEl!.getBoundingClientRect());
		// Load on FIRST open, not on mount: a pane that never opens the picker
		// should not pay a get_available_models round trip per session. A prior
		// failure retries on the next open — a dead child that has since been
		// respawned would otherwise stay "unavailable" for the pane's life.
		if (models() === null) { setFailed(false); void load(); }
	};

	/** Rows matching the filter box, or every row when it is empty. */
	const shown = () => {
		const all = models();
		if (!all) return null;
		const q = query().trim().toLowerCase();
		if (!q) return all;
		return all.filter((m) => `${m.name} ${m.provider} ${m.id}`.toLowerCase().includes(q));
	};

	const load = async () => {
		const data = await ompCommand(props.sessionId, { type: "get_available_models" }, "Model list");
		if (data === null) { setFailed(true); return; }
		setModels(parseModels(data));
	};

	const pickModel = (m: ModelRow) => {
		setAnchor(null);
		void ompCommand(props.sessionId, { type: "set_model", provider: m.provider, modelId: m.id }, "Set model");
	};

	const pickEffort = (level: string) => {
		setAnchor(null);
		void ompCommand(props.sessionId, { type: "set_thinking_level", level }, "Set effort");
	};

	const current = () => models()?.find((m) => `${m.provider}/${m.id}` === props.model);
	// The selected model's ladder when it has one; omp clamps an unsupported
	// level server-side, so a wider-than-real ladder can't produce a bad state.
	const efforts = () => {
		const base = current()?.efforts.length ? current()!.efforts : DEFAULT_EFFORTS;
		return base.includes("off") ? base : [...base, "off"];
	};

	// ctxMenuSurfaceStyle anchors by top-left (it's a cursor menu). The chip sits
	// at the BOTTOM of the viewport, so pin the menu's bottom edge just above the
	// trigger and let it grow upward; a stale `top` would fight that.
	const surfaceStyle = (r: DOMRect): JSX.CSSProperties => {
		const s: JSX.CSSProperties = {
			...ctxMenuSurfaceStyle(0, 0),
			"min-width": "260px",
			"max-height": "50vh",
			"overflow-y": "auto",
			left: `${Math.max(6, r.left)}px`,
			bottom: `${window.innerHeight - r.top + 6}px`,
		};
		delete s.top;
		return s;
	};

	// Same deterministic dismissal as ArrangeMenu: a document click closes unless
	// it landed on the trigger or inside the menu; Escape always closes.
	const onDocClick = (e: MouseEvent) => {
		const t = e.target as Node;
		if (btnEl?.contains(t) || menuEl?.contains(t)) return;
		setAnchor(null);
	};
	const onEsc = (e: KeyboardEvent) => { if (e.key === "Escape") setAnchor(null); };

	onMount(() => {
		document.addEventListener("click", onDocClick);
		document.addEventListener("keydown", onEsc);
		onCleanup(() => {
			document.removeEventListener("click", onDocClick);
			document.removeEventListener("keydown", onEsc);
		});
	});

	return (
		<>
			<button
				ref={btnEl}
				type="button"
				class="omp-composer__model"
				data-testid="omp-chat-model"
				aria-label="Model and effort"
				title={props.model}
				onClick={toggle}
			>
				{label()}
			</button>
			<Show when={anchor()}>
				{(r) => (
					// Portal to <body>: an ancestor <main> carries a transform, so
					// position:fixed would otherwise resolve against <main>'s box.
					<Portal>
						<div ref={menuEl} data-testid="omp-model-menu" class="df-menu-enter" style={surfaceStyle(r())}>
							<Show when={models()?.length}>
								<input
									class="omp-model-menu__filter"
									data-testid="omp-picker-filter"
									type="text"
									placeholder="Filter models"
									aria-label="Filter models"
									value={query()}
									onInput={(e) => setQuery(e.currentTarget.value)}
								/>
							</Show>
							{/* Four distinct empty states: never loaded, load failed, a catalog
							    that really is empty, and a filter that matches nothing —
							    collapsing them would report "loading" forever on the rest. */}
							<Show
								when={shown()?.length}
								fallback={
									<CtxMenuItem testid="omp-model-status" disabled onClick={() => {}}>
										{failed()
											? "Model list unavailable"
											: !models() ? "Loading models…"
											: query() ? "No match" : "No models available"}
									</CtxMenuItem>
								}
							>
								<For each={shown()!}>
									{(m) => (
										<CtxMenuItem
											testid={`omp-model-${m.provider}-${m.id}`}
											onClick={() => pickModel(m)}
										>
											<MenuRow
												checked={`${m.provider}/${m.id}` === props.model}
												label={m.name}
												hint={`${m.provider}/${m.id}`}
											/>
										</CtxMenuItem>
									)}
								</For>
							</Show>
							<CtxMenuSeparator />
							<Show
								when={current()?.reasoning !== false}
								fallback={
									<CtxMenuItem testid="omp-effort-none" disabled onClick={() => {}}>
										No reasoning on this model
									</CtxMenuItem>
								}
							>
								<For each={efforts()}>
									{(level) => (
										<CtxMenuItem testid={`omp-effort-${level}`} onClick={() => pickEffort(level)}>
											<MenuRow checked={level === props.thinkingLevel} label={cap(level)} hint="" />
										</CtxMenuItem>
									)}
								</For>
							</Show>
						</div>
					</Portal>
				)}
			</Show>
		</>
	);
}

/** One picker row: fixed-width check gutter (so unchecked labels stay aligned),
 *  label, dim trailing hint. */
function MenuRow(props: { checked: boolean; label: string; hint: string }) {
	return (
		<div style={{ display: "flex", "align-items": "center", gap: "8px", "white-space": "nowrap" }}>
			<span style={{ width: "16px", flex: "none", display: "inline-flex", "align-items": "center" }}>
				<Show when={props.checked}><Icon name="check" size="sm" /></Show>
			</span>
			<span>{props.label}</span>
			<Show when={props.hint}>
				<span style={{ "margin-left": "auto", "padding-left": "16px", color: "var(--text-lo)" }}>{props.hint}</span>
			</Show>
		</div>
	);
}

// Owns every terminal input path except the textarea controller resource.
// CellTerminal constructs this controller before presentation and renderer.
// Paste, attachments, clipboard, find, history, and pane-local modifier state converge here.
// Renderer constructs TerminalInputController and calls sendControllerData.

import { createSignal, type Accessor, type Setter } from "solid-js";
import { signal } from "@roost/observability/diag";
import {
	buildPtyPayload,
	countLineBreaks,
	CR_BYTES,
	MULTILINE_PASTE_MIN_NEWLINES,
} from "@roost/protocol/terminal-input";
import { enqueueAttachment, pickAndAttachFiles } from "../../lib/attachments.ts";
import { copyToClipboard } from "../../browser/clipboard.ts";
import type { TerminalContext } from "../../voice/keytermContext.ts";
import {
	resolveWorkerPath,
	workerFileHref,
	workerPathPlatform,
} from "../../lib/nativePath.ts";
import { applyCtrlModifier } from "../../client/input/terminalInput.ts";
import {
	clearInput,
	getInputText,
	recordInput,
} from "../../client/input/terminalInputHistory.ts";
import {
	createTerminalFind,
	type TerminalFind,
} from "../../renderer/terminalFindController.ts";
import { registerTerminalFind } from "../../renderer/terminalFindIntent.ts";
import { sendUserTerminalInput } from "../../lib/userTerminalInput.ts";
import type { InputAdmission } from "../../client/carriers/terminal-input-lanes.ts";
import type { ResolveFile } from "../../renderer/terminal-links.ts";
import type { CellTerminalProps } from "./cell-terminal-types.ts";
import type { CellTerminalRuntime } from "./cell-terminal-runtime.ts";

export interface CellTerminalInput {
	ctrlArmed: Accessor<boolean>;
	setCtrlArmed: Setter<boolean>;
	linkActivationArmed: Accessor<boolean>;
	setLinkActivationArmed: Setter<boolean>;
	pendingPaste: Accessor<string | null>;
	setPendingPaste: Setter<string | null>;
	pendingPasteLines(): number;
	sendTerminalText(text: string, submit?: boolean): InputAdmission;
	sendControllerData(data: string): void;
	pasteText(text: string): void;
	enqueueFileItems(items: DataTransferItemList | null | undefined): void;
	attachSelectedFiles(): void;
	copySelectionToClipboard(): Promise<void>;
	pasteFromClipboard(): Promise<void>;
	readonly find: TerminalFind;
	readonly resolveFile: ResolveFile;
	readTerminalContext(): TerminalContext;
	dispose(): void;
}

export function createCellTerminalInput(
	props: CellTerminalProps,
	runtime: CellTerminalRuntime,
): CellTerminalInput {
	const [ctrlArmed, setCtrlArmed] = createSignal(false);
	const [linkActivationArmed, setLinkActivationArmed] = createSignal(false);
	const [pendingPaste, setPendingPaste] = createSignal<string | null>(null);
	const sessionId = runtime.sessionId;
	let disposed = false;
	const clearUncertainPrediction = (): void => {
		runtime.predictor?.clear();
		runtime.renderer?.setPredictedCursor(null);
	};

	const reportImmediateRejection = (
		admission: InputAdmission,
	): admission is { accepted: false; reason: string } => {
		if (admission.accepted) return false;
		clearUncertainPrediction();
		signal("input.drop_burst", {
			sid: sessionId,
			reason: admission.reason,
			cooldownKey: sessionId,
		});
		return true;
	};
	const watchAcceptedAdmission = (admission: InputAdmission): void => {
		if (!admission.accepted) return;
		void admission.result.then((result) => {
			if (result.status === "accepted") {
				runtime.predictor?.noteInputWritten(result.inputSeq);
				return;
			}
			clearUncertainPrediction();
			signal("input.drop_burst", {
				sid: sessionId,
				reason: result.reason,
				cooldownKey: `${sessionId}|${result.status}`,
			});
		});
	};
	const sendTerminalText = (text: string, submit = false): InputAdmission => {
		const payload = text.length === 0
			? new Uint8Array(0)
			: buildPtyPayload(text, runtime.frameBracketedPaste);
		const bytes = submit
			? new Uint8Array(payload.byteLength + CR_BYTES.byteLength)
			: payload;
		if (submit) {
			bytes.set(payload);
			bytes.set(CR_BYTES, payload.byteLength);
		}
		const admission = sendUserTerminalInput(sessionId, bytes, runtime.view?.viewId);
		if (reportImmediateRejection(admission)) return admission;
		if (text.length > 0) recordInput(sessionId, text);
		watchAcceptedAdmission(admission);
		return admission;
	};
	const sendControllerData = (data: string): void => {
		const armed = ctrlArmed();
		const controlledData = armed ? applyCtrlModifier(data) : data;
		if (armed) setCtrlArmed(false);
		const bytes = new TextEncoder().encode(controlledData);
		const admission = sendUserTerminalInput(
			sessionId,
			bytes,
			runtime.view?.viewId,
		);
		if (reportImmediateRejection(admission)) return;
		runtime.predictor?.predict(bytes, admission.inputSeq);
		recordInput(sessionId, controlledData);
		watchAcceptedAdmission(admission);
	};
	const pendingPasteLines = (): number => {
		const text = pendingPaste();
		return text === null ? 0 : countLineBreaks(text) + 1;
	};
	const pasteText = (text: string): void => {
		if (text.length === 0) return;
		if (
			countLineBreaks(text) >= MULTILINE_PASTE_MIN_NEWLINES
			&& !runtime.frameBracketedPaste
		) {
			setPendingPaste(text);
			return;
		}
		sendTerminalText(text);
	};
	const enqueueFileItems = (
		items: DataTransferItemList | null | undefined,
	): void => {
		if (!items) return;
		for (let idx = 0; idx < items.length; idx++) {
			const item = items[idx]!;
			if (item.kind !== "file") continue;
			const file = item.getAsFile();
			if (file) void enqueueAttachment(props.session, file);
		}
	};
	const attachSelectedFiles = (): void => {
		void pickAndAttachFiles(props.session);
	};
	const copySelectionToClipboard = async (): Promise<void> => {
		const text = window.getSelection()?.toString() ?? "";
		if (!text) return;
		await copyToClipboard(text);
	};
	const pasteFromClipboard = async (): Promise<void> => {
		const text = await navigator.clipboard.readText().catch(() => "");
		pasteText(text);
	};
	const resolveFile: ResolveFile = (rawPath, line, fileAuthority) => {
		const workerFingerprint = props.session.worker_fp;
		const cwd = props.session.cwd;
		const platform = workerPathPlatform(workerFingerprint, cwd || rawPath);
		if (rawPath.startsWith("//") && platform !== "win32") return null;
		const localPath = fileAuthority && platform === "win32"
			? `//${fileAuthority}${rawPath.startsWith("/") ? rawPath : `/${rawPath}`}`
			: rawPath;
		const absolutePath = resolveWorkerPath(workerFingerprint, cwd, localPath);
		return absolutePath
			? workerFileHref(workerFingerprint, absolutePath, line)
			: null;
	};
	const find = createTerminalFind({
		sessionId,
		renderer: () => runtime.renderer,
		backfill: () => runtime.backfill,
	});
	const unregisterTerminalFind = registerTerminalFind(sessionId, find);
	const readTerminalContext = (): TerminalContext => ({
		grid: runtime.renderer?.gridText() ?? "",
		scrollback: runtime.renderer?.scrollbackText() ?? "",
		input: getInputText(sessionId),
	});
	const dispose = (): void => {
		if (disposed) return;
		disposed = true;
		unregisterTerminalFind();
		find.dispose();
		clearInput(sessionId);
	};

	return {
		ctrlArmed,
		setCtrlArmed,
		linkActivationArmed,
		setLinkActivationArmed,
		pendingPaste,
		setPendingPaste,
		pendingPasteLines,
		sendTerminalText,
		sendControllerData,
		pasteText,
		enqueueFileItems,
		attachSelectedFiles,
		copySelectionToClipboard,
		pasteFromClipboard,
		find,
		resolveFile,
		readTerminalContext,
		dispose,
	};
}

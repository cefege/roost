import ts from "typescript";
import type { SourceIndex } from "./inventory-source";
import type { InventoryEntry } from "./inventory-types";
import type { OwnerHook, PartUse } from "./inventory-native-types";

/** Component members and the hook-name conventions that decide which leg runs them. */
export const NATIVE_HOOK = /^(?:describe|nativeSheet$)/;
export const TERMINAL_HOOK = /^render/;
export const INPUT_HOOK = /^(?:handleInput|handleNativeEvent|handleMouse|handleKey|onKey|onMouse)/;
export function appendTo<K, V>(map: Map<K, V[]>, key: K, value: V): void {
	const list = map.get(key);
	if (list) list.push(value); else map.set(key, [value]);
}
export const memberName = (source: SourceIndex, member: ts.Node): string => {
	if (ts.isConstructorDeclaration(member)) return "constructor";
	const name = ts.isMethodDeclaration(member) || ts.isPropertyDeclaration(member) || ts.isGetAccessorDeclaration(member) || ts.isSetAccessorDeclaration(member) || ts.isPropertyAssignment(member) || ts.isShorthandPropertyAssignment(member) ? member.name : undefined;
	return (name && source.name(name)) ?? "<anonymous>";
};
export const kindOf = (name: string): PartUse["memberKind"] => NATIVE_HOOK.test(name) ? "native" : INPUT_HOOK.test(name) ? "input" : TERMINAL_HOOK.test(name) ? "terminal" : "other";
export const membersOf = (owner: ts.Node): ts.Node[] => ts.isObjectLiteralExpression(owner) ? [...owner.properties] : ts.isClassLike(owner) ? [...owner.members] : [];
export const usesSuper = (source: SourceIndex, member: ts.Node): boolean => {
	let found = false;
	source.walk(member, node => { if (node.kind === ts.SyntaxKind.SuperKeyword) found = true; });
	return found;
};
export const within = (outer: ts.Node, node: ts.Node): boolean => { for (let current: ts.Node | undefined = node; current; current = current.parent) if (current === outer) return true; return false; };
export const callOf = (argument: ts.Node): ts.Node | undefined => {
	for (let current = argument; current.parent && !ts.isFunctionLike(current); current = current.parent) {
		if ((ts.isCallExpression(current.parent) || ts.isNewExpression(current.parent)) && current.parent.arguments?.includes(current as ts.Expression)) return current.parent;
	}
	return undefined;
};

/** The native hooks an entry declares itself, with the span their body covers. */
export function ownNativeHooks(entry: InventoryEntry): OwnerHook[] {
	const hooks = Array.isArray(entry.details.hooks) ? entry.details.hooks.filter((hook): hook is Record<string, unknown> => typeof hook === "object" && hook !== null) : [];
	return hooks.flatMap(hook => {
		const implementation = hook.implementation;
		if (hook.inherited === true || typeof hook.name !== "string" || !NATIVE_HOOK.test(hook.name) || typeof implementation !== "object" || implementation === null) return [];
		const { source, line, text } = implementation as Record<string, unknown>;
		if (typeof source !== "string" || typeof line !== "number" || typeof text !== "string") return [];
		return [{ name: hook.name, source, line, endLine: line + text.split("\n").length - 1 }];
	});
}

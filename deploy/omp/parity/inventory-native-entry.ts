import ts from "typescript";
import type { SourceIndex } from "./inventory-source";
import { within } from "./inventory-native-members";

type Parents = Map<ts.Node, Set<ts.Node>>;
export interface Entry {
	/** Call arguments that bound the traced value to a parameter of the function. */
	arguments: ts.Node[];
	/** The value also arose inside the function from a source the flow cannot attribute, so any caller may run it. */
	callers: boolean;
	/** Origins in an enclosing function the value was captured from; that function's activation is where the value lives. */
	captured: Array<{ origin: ts.Node; scope: ts.Node }>;
}

/** How the value traced through `parents` got from outside `fn` to `start` inside it. */
export function entryArguments(source: SourceIndex, start: ts.Node, fn: ts.Node, parents: Parents): Entry {
	const found = new Set<ts.Node>();
	const visited = new Set<ts.Node>();
	const captured: Entry["captured"] = [];
	let callers = false;
	const bindsParameter = (node: ts.Node): boolean => {
		const declaration = ts.isIdentifier(node) ? source.declaration(node) : undefined;
		const parameter = declaration && ts.isBindingElement(declaration) ? declaration.parent.parent : declaration;
		return !!parameter && ts.isParameter(parameter) && parameter.parent === fn;
	};
	const visit = (node: ts.Node): void => {
		if (visited.has(node)) return;
		visited.add(node);
		const origins = parents.get(node);
		if (!origins?.size) { callers = true; return; }
		for (const origin of origins) {
			if (within(fn, origin)) visit(origin);
			else if (bindsParameter(node)) found.add(origin);
			else {
				let scope: ts.Node | undefined = origin.parent;
				while (scope && !ts.isFunctionLike(scope)) scope = scope.parent;
				if (scope && within(scope, fn)) captured.push({ origin, scope });
				else callers = true;
			}
		}
	};
	visit(start);
	return { arguments: [...found], callers, captured };
}

import { realpath } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

export interface SourceLoader { <Module>(sourceRelativePath: string): Promise<Module> }

export function sourceLoader(sourceRoot: string): SourceLoader {
  return async function load<Module>(sourceRelativePath: string): Promise<Module> {
    if (!sourceRelativePath.startsWith("packages/") || isAbsolute(sourceRelativePath)) throw new Error(`Invalid owned source module: ${sourceRelativePath}`);
    const root = await realpath(sourceRoot);
    const modulePath = await realpath(resolve(root, sourceRelativePath));
    const local = relative(root, modulePath);
    if (local === ".." || local.startsWith(`..${sep}`) || isAbsolute(local)) throw new Error(`Source module escapes owned checkout: ${sourceRelativePath}`);
    // Each isolated CLI selects its owned checkout at runtime; importing here keeps source singletons out of the runner.
    return await import(pathToFileURL(modulePath).href) as Module;
  };
}

// Type shim for `import BRIDGE_SOURCE from "./bridge-extension.js" with { type: "text" }`
// in bridge-install.ts. The extension is plain JS on purpose (it is loaded inside
// omp's runtime, not Roost's) and is imported as TEXT so `bun build --compile`
// inlines the body into the binary — the source tree is absent at runtime there.
declare const source: string;
export default source;

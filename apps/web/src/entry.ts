// Security boundary: these static imports have no dependencies and perform no
// network I/O before the scrub. The SPA graph is requested only after the
// current URL has been synchronously stripped of fragment/query-shaped
// credentials, and only after the serving origin's local bootstrap has been
// resolved — connect.ts picks its coordinator at module scope, so a bootstrap
// that landed later would be ignored.
import { captureAndScrubFragmentCredential } from "./auth/fragment-credential.ts";
import { loadLocalBootstrap } from "./lib/localBootstrap.ts";

captureAndScrubFragmentCredential();

// Chained rather than a top-level `await`: TLA leaves this module's exports in
// their temporal dead zone until the probe settles, so anything touching
// `mainModulePromise` during that window throws. The chain keeps the ordering
// guarantee — main.tsx is requested only after the bootstrap resolved — while
// the binding itself initializes synchronously.
export const mainModulePromise = loadLocalBootstrap().then(() => import("./main.tsx"));

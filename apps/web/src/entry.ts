// Security boundary: this static import has no dependencies and performs no I/O.
// The SPA graph is requested only after the current URL has been synchronously
// stripped of fragment/query-shaped credentials.
import { captureAndScrubFragmentCredential } from "./auth/fragment-credential.ts";

captureAndScrubFragmentCredential();

export const mainModulePromise = import("./main.tsx");

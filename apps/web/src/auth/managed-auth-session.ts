// Browser-retained managed authentication ceremonies must end together on logout.
// Explicit logout and peer-tab key revocation call this leaf before navigation.
// Fragment proofs, completion progress, and pending route transactions remain owner-cleared.

import { clearCapturedFragmentCredentialsForLogout } from "./fragment-credential.ts";
import { clearManagedAuthProgressForLogout } from "./managed-auth-progress.ts";
import { clearPendingTenantRouteSwitchForLogout } from "./tenant-routing.ts";

export function clearManagedAuthCeremoniesForLogout(): void {
  clearCapturedFragmentCredentialsForLogout();
  clearManagedAuthProgressForLogout();
  clearPendingTenantRouteSwitchForLogout();
}

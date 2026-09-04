// Admits every managed SaaS CLI dispatch under its exact operating identity.
// The root CLI checks public and hidden profiles before loading their runtime modules.
// Keeping this at the process boundary preserves service and container sandbox identities.
import { userInfo } from "node:os";

export type SaasEntryProfile = "operator" | "provisioner" | "auth" | "instance";

export interface SaasEntryIdentity {
  platform: NodeJS.Platform;
  username: string;
  uid: number;
  effectiveUid: number;
  gid: number;
  effectiveGid: number;
  managedContainer: string | undefined;
  saasMode: string | undefined;
  trustProxy: string | undefined;
  coordinatorBind: string | undefined;
  publicBind: string | undefined;
}

function refuse(profile: SaasEntryProfile): never {
  throw new Error(`SaaS ${profile} entry is not admitted on this host`);
}

/** Test seam for proving profile rejection without changing process identity. */
export function _assertSaasEntryAdmission(
  profile: SaasEntryProfile,
  identity: Readonly<SaasEntryIdentity>,
): void {
  if (identity.platform !== "linux") refuse(profile);
  const ids = [identity.uid, identity.effectiveUid, identity.gid, identity.effectiveGid];
  if (ids.some((id) => !Number.isSafeInteger(id) || id < 0)) refuse(profile);

  if (profile === "operator" || profile === "provisioner") {
    if (identity.uid !== 0 || identity.effectiveUid !== 0 || identity.username !== "root") {
      refuse(profile);
    }
    return;
  }
  if (profile === "auth") {
    if (
      identity.username !== "roost-signup"
      || identity.uid === 0
      || identity.uid !== identity.effectiveUid
      || identity.gid !== identity.effectiveGid
    ) {
      refuse(profile);
    }
    return;
  }
  if (
    identity.uid === 0
    || identity.uid !== identity.effectiveUid
    || identity.gid !== identity.effectiveGid
    || identity.managedContainer !== "1"
    || identity.saasMode !== "1"
    || identity.trustProxy !== "1"
    || identity.coordinatorBind !== "127.0.0.1:4103"
    || identity.publicBind !== "0.0.0.0:4104"
  ) {
    refuse(profile);
  }
}

export function assertSaasEntryAdmission(profile: SaasEntryProfile): void {
  const account = userInfo();
  _assertSaasEntryAdmission(profile, {
    platform: process.platform,
    username: account.username,
    uid: process.getuid?.() ?? -1,
    effectiveUid: process.geteuid?.() ?? -1,
    gid: process.getgid?.() ?? -1,
    effectiveGid: process.getegid?.() ?? -1,
    managedContainer: process.env.ROOST_MANAGED_CONTAINER,
    saasMode: process.env.ROOST_SAAS_MODE,
    trustProxy: process.env.ROOST_TRUST_PROXY,
    coordinatorBind: process.env.ROOST_COORDINATOR_BIND,
    publicBind: process.env.ROOST_PUBLIC_BIND,
  });
}

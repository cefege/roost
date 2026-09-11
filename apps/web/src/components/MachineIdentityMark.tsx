// MachineIdentityMark renders the safe presentation selected from a static Worker
// record. Folder rows use it as their leading mark; Linux marks are local and
// generic platforms stay on the shared Icon primitive.

import { createMemo, type JSX } from "solid-js";
import type { MachineIdentityInput } from "../lib/machineIdentity.ts";
import { machineIdentityPresentation } from "../lib/machineIdentity.ts";
import { Icon } from "./Settings/md/Icon.tsx";
import { LinuxDistributionMark } from "./LinuxDistributionMark.tsx";

export function MachineIdentityMark(props: {
  worker: MachineIdentityInput | undefined;
  contextTitle?: string;
}): JSX.Element {
  const presentation = createMemo(() => machineIdentityPresentation(props.worker));
  const title = createMemo(() => props.contextTitle
    ? `${presentation().title} · ${props.contextTitle}`
    : presentation().title);
  return (
    <span
      class="machine-identity-mark"
      data-linux-brand={presentation().linuxBrand ?? undefined}
      role="img"
      aria-label={presentation().title}
      title={title()}
    >
      {presentation().linuxBrand
        ? <LinuxDistributionMark brand={presentation().linuxBrand!} />
        : <Icon name={presentation().icon} size="sm" />}
      {presentation().appleChipBadge ? <span class="machine-identity-mark__chip">{presentation().appleChipBadge}</span> : null}
    </span>
  );
}

// DevicesPane — Settings → Devices. Two ways to grant access to Roost:
// pair a phone (scan a QR) and approve another browser that requested in.
// Merges the former separate "Pair" (PairDevicePane) + "Pairing"
// (Onboarding) rail items into one pane with two Card sections.
// Callers: SettingsRoot.tsx. Depends on: PairDevicePane, Onboarding.

import { Card } from "./md/primitives.tsx";
import { PairDevicePane } from "./PairDevicePane.tsx";
import { Onboarding } from "../Onboarding.tsx";

export function DevicesPane() {
  return (
    <div data-testid="settings-devices-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <Card
        title="Pair a phone"
        supporting="Scan this with your phone's camera (it must be on your tailnet). Roost opens and pairs automatically — no typing."
      >
        <PairDevicePane />
      </Card>
      <Card
        title="Approve a browser"
        supporting="When you open Roost in a new browser it requests access. Approve the pending request here from a browser that's already paired."
      >
        <Onboarding embedded />
      </Card>
    </div>
  );
}

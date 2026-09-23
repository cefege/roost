// PairingRequesterProvider owns the document's single requester ceremony.
// App.tsx mounts it above the access gate so checking → unauthorized →
// authorized transitions never dispose the controller or clear its tab record;
// Onboarding's pairing page reads it through usePairingRequester().

import { createContext, createSignal, useContext } from "solid-js";
import type { Accessor, JSX } from "solid-js";
import { createOnboardingPairingCeremony } from "./onboarding-pairing-ceremony.ts";
import type { OnboardingPairingCeremony } from "./onboarding-pairing-ceremony.ts";

export interface PairingRequester extends OnboardingPairingCeremony {
  /** Last terminal requester failure; null until one occurs or after clearing. */
  requestError: Accessor<string | null>;
  clearRequestError: () => void;
}

const PairingRequesterContext = createContext<PairingRequester>();

export function usePairingRequester(): PairingRequester {
  const context = useContext(PairingRequesterContext);
  if (context === undefined) throw new Error("Pairing requester provider is unavailable.");
  return context;
}

export function PairingRequesterProvider(props: { children?: JSX.Element }): JSX.Element {
  const [requestError, setRequestError] = createSignal<string | null>(null);
  const ceremony = createOnboardingPairingCeremony({
    redirectAfterPairing: () => window.location.replace("/"),
    reportRequestError: setRequestError,
  });
  const requester: PairingRequester = {
    ...ceremony,
    requestError,
    clearRequestError: () => setRequestError(null),
  };
  return (
    <PairingRequesterContext.Provider value={requester}>
      {props.children}
    </PairingRequesterContext.Provider>
  );
}

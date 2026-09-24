// Content of the unauthorized pairing page: header, the one primary Request
// approval card, and the collapsed Other pairing options. Onboarding renders it
// while rootStore.browser_access_state is "unauthorized"; the ceremony itself
// belongs to PairingRequesterProvider so it survives the gate switching away.

import { Show } from "solid-js";
import type { JSX } from "solid-js";
import { OnboardingRequestCard } from "./OnboardingRequestCard.tsx";
import { PairingOtherOptions } from "./PairingOtherOptions.tsx";
import { usePairingRequester } from "./PairingRequesterProvider.tsx";
import { PairingPageHeader } from "./PairingPageHeader.tsx";
import { PairingStatusNotice } from "./PairingStatusNotice.tsx";

export function PairingGatePanel(): JSX.Element {
  const requester = usePairingRequester();

  function requestApproval(): void {
    requester.clearRequestError();
    void requester.start();
  }

  return (
    <>
      <PairingPageHeader
        title="Pair this browser"
        body="This browser needs approval before it can access your Roost workspace."
      />
      <OnboardingRequestCard
        ephemeralId={requester.ephemeralId()}
        pollStatus={requester.pollStatus()}
        verificationCode={requester.verificationCode()}
        requestFailed={requester.requestError() !== null}
        confirmationError={requester.confirmationError()}
        busy={requester.busy()}
        onStart={requestApproval}
        onVerificationCodeInput={requester.updateVerificationCode}
        onConfirm={() => void requester.confirm()}
      />
      <Show when={requester.requestError()}>
        {(message) => (
          <PairingStatusNotice tone="error" message={message()} testId="onboarding-request-error" />
        )}
      </Show>
      <PairingOtherOptions requester={requester} />
    </>
  );
}

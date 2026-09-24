// MachineLocalAccessGuide — presents the operator-managed HTTPS expansion path.
// MachineDeployDialog renders it when identity discovery finds only local access.
// It owns no RPC or transition state; the dialog supplies refresh ownership.

import { Show } from "solid-js";
import { Button, Card } from "../Settings/md/primitives.tsx";

interface MachineLocalAccessGuideProps {
  checking: boolean;
  onRecheck: () => void;
}

const STACK_STYLE = {
  display: "grid",
  gap: "var(--md-space-4)",
} as const;

const SUPPORT_STYLE = {
  margin: 0,
  color: "var(--md-sys-color-on-surface-variant)",
} as const;

const LIST_STYLE = {
  display: "grid",
  gap: "var(--md-space-4)",
  margin: 0,
  padding: "0 0 0 var(--md-space-5)",
} as const;

const LIST_ITEM_STYLE = {
  padding: "0 0 0 var(--md-space-1)",
} as const;

const CODE_STYLE = {
  "overflow-wrap": "anywhere",
} as const;

export function MachineLocalAccessGuide(props: MachineLocalAccessGuideProps) {
  return (
    <Card
      data-testid="machine-deploy-local-only"
      title="Connect another machine"
      supporting="The new machine needs a reachable HTTPS address for this Roost."
    >
      <div style={STACK_STYLE}>
        <Show
          when={props.checking}
          fallback={(
            <>
              <ol style={LIST_STYLE}>
                <li style={LIST_ITEM_STYLE}>
                  <p class="md-body-m" style={SUPPORT_STYLE}>
                    Choose an HTTPS address supplied by the operator&apos;s proxy/tunnel/private-access setup.
                    No purchased domain is required; Tailscale Serve can supply a <code>*.ts.net</code> address.
                    Network membership alone does not expose Roost.
                  </p>
                </li>
                <li style={LIST_ITEM_STYLE}>
                  <p class="md-body-m" style={SUPPORT_STYLE}>
                    Before exposing the running local listener, run{" "}
                    <code style={CODE_STYLE}>roost quickstart --coordinator-url https://&lt;your-Roost-address&gt;</code>{" "}
                    on the coordinator machine. This first switches Roost to the proxy trust profile.
                  </p>
                </li>
                <li style={LIST_ITEM_STYLE}>
                  <div style={STACK_STYLE}>
                    <p class="md-body-m" style={SUPPORT_STYLE}>
                      Configure the front door to forward to the installed loopback bind and overwrite XFF. Follow the{" "}
                      <a
                        href="https://github.com/cefege/roost/blob/main/GETTING_STARTED.md#three-coordinator-httptls-front-door-recipes"
                        target="_blank"
                        rel="noreferrer"
                      >
                        coordinator HTTP/TLS front-door recipes
                      </a>
                      . Tailscale Serve is an optional default-port example:
                    </p>
                    <code class="md-body-s" style={CODE_STYLE}>
                      tailscale serve --bg --https=443 http://127.0.0.1:4103
                    </code>
                    <p class="md-body-m" style={SUPPORT_STYLE}>
                      Then run <code>tailscale serve status</code>. An operator-changed loopback port must replace{" "}
                      <code>4103</code>. Both machines must have the required tailnet route/ACL access; WireGuard alone
                      does not supply HTTPS.
                    </p>
                  </div>
                </li>
                <li style={LIST_ITEM_STYLE}>
                  <p class="md-body-m" style={SUPPORT_STYLE}>
                    Return to this dialog and choose <strong>Check again</strong>. Generate the join command only after
                    the coordinator advertises a valid external origin. The target must reach that address, and the main
                    machine must remain awake and available for remote control.
                  </p>
                </li>
              </ol>
              <div>
                <Button
                  variant="secondary"
                  data-testid="machine-deploy-recheck"
                  onClick={props.onRecheck}
                >
                  Check again
                </Button>
              </div>
            </>
          )}
        >
          <p class="md-body-m" data-testid="machine-deploy-pending" style={SUPPORT_STYLE}>
            Checking the coordinator&apos;s enrollment address…
          </p>
        </Show>
      </div>
    </Card>
  );
}

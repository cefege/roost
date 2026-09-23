// Unified approval card for pending browser pair requests.
// Onboarding and PairRequestNotifier delegate here so request provenance and
// approval controls stay identical on every surface. It composes the M3
// settings primitives and removes expired requests even when a delta is late.
// Raw user agent and request ID sit under a per-instance Technical details toggle.

import { createSignal, createUniqueId, onCleanup, Show } from "solid-js";
import { Card, Chip, Button, StatusDot, List, ListRow } from "./Settings/md/primitives.tsx";
import type { PairRequest } from "../store/root.ts";

export interface PairRequestCardProps {
  request: PairRequest;
  onApprove: () => void;
  onDeny: () => void;
  busy?: boolean;
}

/** Returns true only for populated expiry timestamps that have elapsed. */
export function isPairRequestExpired(request: PairRequest, now = Date.now()): boolean {
  const expiresAtMs = Number(request.expiresAtMs ?? 0);
  return expiresAtMs > 0 && expiresAtMs <= now;
}

export function PairRequestCard(props: PairRequestCardProps) {
  const [now, setNow] = createSignal(Date.now());
  const expiryTimer = setInterval(() => setNow(Date.now()), 1_000);
  onCleanup(() => clearInterval(expiryTimer));
  // The same request can render on the home page and in the notification dock
  // at once, so the disclosure id must be unique per card instance.
  const technicalDetailsId = createUniqueId();
  const [technicalDetailsOpen, setTechnicalDetailsOpen] = createSignal(false);

  const deviceLabel = () => {
    const deviceParts = [props.request.clientBrowser, props.request.clientOs]
      .map((value) => value?.trim())
      .filter((value): value is string => Boolean(value));
    return deviceParts.join(" · ") || props.request.label?.trim() || "Unknown device";
  };
  const locationLabel = () => [props.request.city, props.request.region, props.request.countryCode]
    .map((value) => value?.trim())
    .filter((value): value is string => Boolean(value))
    .join(", ") || "Location unavailable";
  const networkLabel = () => {
    const sourceIp = props.request.sourceIp?.trim();
    return sourceIp ? `IP ${sourceIp}` : "IP unavailable";
  };
  const relativeAge = () => formatRelativeAge(Number(props.request.created_at_ms ?? 0), now());
  const expiryLabel = () => formatExpiry(Number(props.request.expiresAtMs ?? 0), now());

  return (
    <Show when={!isPairRequestExpired(props.request, now())}>
      <div
        data-testid="pair-request-card"
        data-ephemeral-id={props.request.ephemeral_id}
        style={{ width: "100%" }}
      >
        <Card
          title="New browser wants to pair"
          supporting="Review the browser and network details before allowing access."
          variant="elevated"
        >
          <List contained>
            <ListRow
              leading="devices"
              testId="pair-request-device"
              headline={<span class="md-body-m">{deviceLabel()}</span>}
              trailing={
                <Show when={props.request.clientDeviceType?.trim()}>
                  <Chip label={props.request.clientDeviceType.trim()} />
                </Show>
              }
            />
            <ListRow
              leading="location_on"
              testId="pair-request-location"
              headline={<span class="md-body-m">{locationLabel()}</span>}
            />
            <ListRow
              leading="lan"
              testId="pair-request-network"
              headline={<span class="md-body-m">{networkLabel()}</span>}
            />
            <ListRow
              leading="verified_user"
              testId="pair-request-identity"
              headline={
                <span
                  class="md-body-m"
                  style={{ display: "inline-flex", "align-items": "center", gap: "var(--md-space-2)", "min-width": "0", "overflow-wrap": "anywhere" }}
                >
                  <Show
                    when={props.request.edgeIdentityVerified && props.request.edgeIdentity?.trim()}
                    fallback={
                      <Show
                        when={props.request.edgeIdentity?.trim()}
                        fallback={
                          <>
                            <StatusDot status="idle" title="No identity" />
                            <span>No front-door identity</span>
                          </>
                        }
                      >
                        <StatusDot status="warn" title="Identity claim" />
                        <span>Claimed identity {props.request.edgeIdentity}</span>
                      </Show>
                    }
                  >
                    <StatusDot status="ok" title="Verified identity" />
                    <span>Signed in as {props.request.edgeIdentity}</span>
                  </Show>
                </span>
              }
            />
            <ListRow
              leading="schedule"
              testId="pair-request-expiry"
              headline={<span class="md-body-m">{expiryLabel()}</span>}
              support={<span class="md-body-s">{relativeAge()}</span>}
            />
          </List>
          <div>
            <Button
              variant="ghost"
              size="sm"
              icon={technicalDetailsOpen() ? "expand_less" : "expand_more"}
              aria-expanded={technicalDetailsOpen()}
              aria-controls={technicalDetailsId}
              data-testid="pair-request-technical-details-toggle"
              onClick={() => setTechnicalDetailsOpen((open) => !open)}
            >
              Technical details
            </Button>
          </div>
          <Show when={technicalDetailsOpen()}>
            <div id={technicalDetailsId}>
              <List contained>
                <ListRow
                  leading="language"
                  testId="pair-request-user-agent"
                  headline={<span class="md-label-m">User agent</span>}
                  support={
                    <span
                      class="md-body-s"
                      style={{
                        display: "-webkit-box",
                        "-webkit-box-orient": "vertical",
                        "-webkit-line-clamp": "3",
                        overflow: "hidden",
                        "overflow-wrap": "anywhere",
                        "white-space": "pre-wrap",
                        "user-select": "text",
                      }}
                    >
                      {props.request.userAgent || "User agent unavailable"}
                    </span>
                  }
                />
                <ListRow
                  leading="key"
                  testId="pair-request-id"
                  headline={<span class="md-label-m">Request ID</span>}
                  support={
                    <code class="md-body-s" style={{ "overflow-wrap": "anywhere", "user-select": "text" }}>
                      {props.request.ephemeral_id}
                    </code>
                  }
                />
              </List>
            </div>
          </Show>
          <div
            role="group"
            aria-label="Pair request actions"
            style={{
              display: "flex",
              "justify-content": "flex-end",
              gap: "var(--md-space-2)",
            }}
          >
            <Button
              variant="outline"
              icon="close"
              data-testid="pair-card-dismiss"
              disabled={props.busy}
              onClick={() => props.onDeny()}
            >
              Deny
            </Button>
            <Button
              variant="default"
              icon="check"
              data-testid="pair-card-approve"
              disabled={props.busy}
              onClick={() => props.onApprove()}
            >
              {props.busy ? "Approving…" : "Approve"}
            </Button>
          </div>
        </Card>
      </div>
    </Show>
  );
}

function formatRelativeAge(createdAtMs: number, now: number): string {
  if (!Number.isFinite(createdAtMs) || createdAtMs <= 0) return "Request age unavailable";
  const ageMs = Math.max(0, now - createdAtMs);
  if (ageMs < 60_000) return "Requested just now";
  if (ageMs < 3_600_000) return `Requested ${Math.floor(ageMs / 60_000)}m ago`;
  if (ageMs < 86_400_000) return `Requested ${Math.floor(ageMs / 3_600_000)}h ago`;
  return `Requested ${Math.floor(ageMs / 86_400_000)}d ago`;
}

function formatExpiry(expiresAtMs: number, now: number): string {
  if (!Number.isFinite(expiresAtMs) || expiresAtMs <= 0) return "Expiry unavailable";
  return `Expires in ${Math.max(0, Math.ceil((expiresAtMs - now) / 60_000))}m`;
}

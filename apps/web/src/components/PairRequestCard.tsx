// Unified approval card for pending browser pair requests.
// Onboarding and PairRequestNotifier delegate here so request provenance and
// approval controls stay identical on every surface. It composes the M3
// settings primitives and removes expired requests even when a delta is late.

import { createSignal, onCleanup, Show } from "solid-js";
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
        <Card title="New browser wants to pair" variant="elevated">
          <List contained>
            <ListRow
              testId="pair-request-device"
              headline={<span class="md-body-m">{deviceLabel()}</span>}
              trailing={
                <Show when={props.request.clientDeviceType?.trim()}>
                  <Chip label={props.request.clientDeviceType.trim()} />
                </Show>
              }
            />
            <ListRow
              testId="pair-request-location"
              headline={<span class="md-body-m">{locationLabel()}</span>}
            />
            <ListRow
              testId="pair-request-network"
              headline={<span class="md-body-m">{networkLabel()}</span>}
            />
            <ListRow
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
              testId="pair-request-code"
              headline={<span class="md-body-m">Code: {props.request.ephemeral_id}</span>}
              support={<span class="md-body-s">{relativeAge()} · {expiryLabel()}</span>}
            />
          </List>
          <div
            style={{
              display: "flex",
              "justify-content": "flex-end",
              gap: "var(--md-space-2)",
              "margin-top": "var(--md-space-1)",
            }}
          >
            <Button
              variant="text"
              data-testid="pair-card-dismiss"
              disabled={props.busy}
              onClick={() => props.onDeny()}
            >
              Deny
            </Button>
            <Button
              variant="filled"
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
  if (!Number.isFinite(createdAtMs) || createdAtMs <= 0) return "Age unavailable";
  const ageMs = Math.max(0, now - createdAtMs);
  if (ageMs < 60_000) return "just now";
  if (ageMs < 3_600_000) return `${Math.floor(ageMs / 60_000)}m ago`;
  if (ageMs < 86_400_000) return `${Math.floor(ageMs / 3_600_000)}h ago`;
  return `${Math.floor(ageMs / 86_400_000)}d ago`;
}

function formatExpiry(expiresAtMs: number, now: number): string {
  if (!Number.isFinite(expiresAtMs) || expiresAtMs <= 0) return "Expiry unavailable";
  return `Expires in ${Math.max(0, Math.ceil((expiresAtMs - now) / 60_000))}m`;
}

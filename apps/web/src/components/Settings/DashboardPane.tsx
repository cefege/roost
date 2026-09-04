// Dashboard settings must stay scoped to the coordinator-confirmed dashboard selection.
// The settings shell renders this pane as the entry point to that dashboard's machines.
// Root-store state and the shared scope selector supply the server-backed context.

import { createMemo, For, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { DashboardScopeSelector } from "../layout/DashboardScopeSelector.tsx";
import { rootStore } from "../../store/root.ts";
import { Button, Card, Chip } from "./md/primitives.tsx";

/** Dashboard settings use only the selected scope returned by the coordinator.
 * Existing Machines is therefore the dashboard's real worker-management entry,
 * rather than a client-filtered global worker list. */
export function DashboardPane() {
  const navigate = useNavigate();
  const dashboard = createMemo(() => {
    const id = rootStore.selected_dashboard_id;
    return id ? rootStore.dashboards[id] ?? null : null;
  });
  const organization = createMemo(() => {
    const current = dashboard();
    return current ? rootStore.organizations[current.organization_id] ?? null : null;
  });

  return (
    <div data-testid="settings-dashboard-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <DashboardScopeSelector />
      <Card
        title={dashboard()?.name ?? "Dashboard"}
        supporting="All runtime data in this settings area is scoped by the selected dashboard on the coordinator."
      >
        <Show
          when={dashboard()}
          fallback={<p style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>Select a dashboard to view its settings.</p>}
        >
          {(current) => (
            <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}>
              <div class="md-body-m">Organization: {organization()?.name ?? current().organization_id}</div>
              <div class="md-body-m">Dashboard role: {current().dashboard_role}</div>
              <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>Slug: {current().slug}</div>
              <Button variant="filled" data-testid="dashboard-machines-entry" onClick={() => navigate("/settings/machines")}>
                Manage dashboard machines
              </Button>
            </div>
          )}
        </Show>
      </Card>
      <Card title="Effective capabilities" supporting="Capabilities are computed by the coordinator for this selected membership.">
        <Show
          when={rootStore.effective_capabilities.length > 0}
          fallback={<p style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>No capabilities were returned.</p>}
        >
          <div style={{ display: "flex", gap: "var(--md-space-2)", "flex-wrap": "wrap" }}>
            <For each={rootStore.effective_capabilities}>
              {(capability) => (
                <Chip label={capability} testId={`dashboard-capability-${capability}`} />
              )}
            </For>
          </div>
        </Show>
      </Card>
    </div>
  );
}

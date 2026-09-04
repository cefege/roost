// Organization settings must reflect coordinator membership rather than invented client state.
// The settings shell renders this pane for the currently selected dashboard scope.
// Root-store dashboards and the shared scope selector provide its authoritative data.

import { createMemo, For, Show } from "solid-js";
import { DashboardScopeSelector } from "../layout/DashboardScopeSelector.tsx";
import { rootStore } from "../../store/root.ts";
import { Card, List, ListRow } from "./md/primitives.tsx";

/** Server-backed organization membership view. Management actions intentionally
 * remain absent until their coordinator RPCs exist; this surface never invents
 * local authorization or membership state. */
export function OrganizationPane() {
  const selectedDashboard = createMemo(() => {
    const id = rootStore.selected_dashboard_id;
    return id ? rootStore.dashboards[id] ?? null : null;
  });
  const organization = createMemo(() => {
    const dashboard = selectedDashboard();
    return dashboard ? rootStore.organizations[dashboard.organization_id] ?? null : null;
  });
  const organizationDashboards = createMemo(() => {
    const id = organization()?.id;
    if (!id) return [];
    return Object.values(rootStore.dashboards)
      .filter((dashboard) => dashboard.organization_id === id)
      .sort((a, b) => a.name.localeCompare(b.name));
  });

  return (
    <div data-testid="settings-organization-pane" style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-5)" }}>
      <DashboardScopeSelector />
      <Card
        title={organization()?.name ?? "Organization"}
        supporting="The coordinator returns the organizations and dashboards available to this browser."
      >
        <Show
          when={organization()}
          fallback={<p style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>No organization access is currently available.</p>}
        >
          {(current) => (
            <div style={{ display: "flex", "flex-direction": "column", gap: "var(--md-space-2)" }}>
              <div class="md-body-m">Role: {current().role}</div>
              <div class="md-body-s" style={{ color: "var(--md-sys-color-on-surface-variant)" }}>Slug: {current().slug}</div>
            </div>
          )}
        </Show>
      </Card>
      <Card
        title="Accessible dashboards"
        supporting="Choose a dashboard above to load only its machines, terminals, workspaces, and settings."
      >
        <Show
          when={organizationDashboards().length > 0}
          fallback={<p style={{ margin: "0", color: "var(--md-sys-color-on-surface-variant)" }}>No dashboard memberships in this organization.</p>}
        >
          <List contained>
            <For each={organizationDashboards()}>
              {(dashboard) => (
                <ListRow
                  testId={`organization-dashboard-${dashboard.id}`}
                  headline={dashboard.name}
                  support={`Role: ${dashboard.dashboard_role}`}
                />
              )}
            </For>
          </List>
        </Show>
      </Card>
    </div>
  );
}

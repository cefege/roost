// Dashboard-scoped screens need one selector that cannot invent unauthorized memberships.
// The shell and settings panes use this component to change the active organization or dashboard.
// Coordinator-confirmed root state and server selection mutations remain authoritative.

import { createEffect, createMemo, createSignal, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { rootStore } from "../../store/root.ts";
import { selectDashboardFromServer } from "../../store/dashboard-selection.ts";
import { settingsScopeSelectorVisible } from "../Settings/settingsNavigation.ts";
import { Button, Select, Surface } from "../Settings/md/primitives.tsx";

function scopeError(error: unknown): string {
  return error instanceof Error ? error.message : "Could not change dashboard";
}

/**
 * The shell's scope picker renders only memberships returned by the
 * coordinator. Changing either control asks AuthDashboardAccess to confirm the
 * candidate before the selected-dashboard header or any dashboard data moves.
 */
export function DashboardScopeSelector() {
  const navigate = useNavigate();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const organizations = createMemo(() =>
    Object.values(rootStore.organizations).slice().sort((a, b) => a.name.localeCompare(b.name)),
  );
  const selectedDashboard = createMemo(() => {
    const id = rootStore.selected_dashboard_id;
    return id ? rootStore.dashboards[id] ?? null : null;
  });
  const [displayedOrganizationId, setDisplayedOrganizationId] = createSignal(
    selectedDashboard()?.organization_id ?? "",
  );
  const [displayedDashboardId, setDisplayedDashboardId] = createSignal(
    selectedDashboard()?.id ?? "",
  );
  const dashboards = createMemo(() =>
    Object.values(rootStore.dashboards)
      .filter((dashboard) => dashboard.organization_id === displayedOrganizationId())
      .sort((a, b) => a.name.localeCompare(b.name)),
  );
  const organizationOptions = createMemo(() =>
    organizations().map((organization) => ({
      value: organization.id,
      label: organization.name,
    })),
  );
  const dashboardOptions = createMemo(() =>
    dashboards().map((dashboard) => ({
      value: dashboard.id,
      label: dashboard.name,
    })),
  );

  function restoreDisplayedScope(): void {
    const confirmedDashboard = selectedDashboard();
    setDisplayedOrganizationId(confirmedDashboard?.organization_id ?? "");
    setDisplayedDashboardId(confirmedDashboard?.id ?? "");
  }

  createEffect(() => {
    if (!busy()) restoreDisplayedScope();
  });

  async function selectDashboard(dashboardId: string): Promise<void> {
    const candidateDashboard = rootStore.dashboards[dashboardId];
    if (!candidateDashboard || dashboardId === rootStore.selected_dashboard_id || busy()) {
      restoreDisplayedScope();
      return;
    }
    setBusy(true);
    setDisplayedOrganizationId(candidateDashboard.organization_id);
    setDisplayedDashboardId(candidateDashboard.id);
    setError(null);
    try {
      if (!await selectDashboardFromServer(dashboardId)) {
        setError("The coordinator did not confirm that dashboard.");
      }
    } catch (cause) {
      setError(scopeError(cause));
    } finally {
      restoreDisplayedScope();
      setBusy(false);
    }
  }

  function selectOrganization(organizationId: string): void {
    if (!organizationId || organizationId === displayedOrganizationId() || busy()) {
      restoreDisplayedScope();
      return;
    }
    setDisplayedOrganizationId(organizationId);
    const firstDashboard = Object.values(rootStore.dashboards)
      .filter((dashboard) => dashboard.organization_id === organizationId)
      .sort((a, b) => a.name.localeCompare(b.name))[0];
    if (firstDashboard) {
      void selectDashboard(firstDashboard.id);
    } else {
      restoreDisplayedScope();
    }
  }

  return (
    <Show when={settingsScopeSelectorVisible(rootStore.coord_identity?.saas_mode) && organizations().length > 0 && selectedDashboard()}>
      <section data-testid="dashboard-scope-selector" aria-label="Current organization and dashboard">
        <Surface
          level={1}
          radius="md"
          pad={3}
          border
          style={{
            display: "flex",
            "flex-direction": "column",
            gap: "var(--md-space-2)",
          }}
        >
          <div style={{ display: "grid" }}>
            <Select
              testId="organization-selector"
              label="Organization"
              value={displayedOrganizationId()}
              disabled={busy()}
              onChange={selectOrganization}
              options={organizationOptions()}
            />
          </div>
          <div style={{ display: "grid" }}>
            <Select
              testId="dashboard-selector"
              label="Dashboard"
              value={displayedDashboardId()}
              disabled={busy()}
              onChange={(dashboardId) => void selectDashboard(dashboardId)}
              options={dashboardOptions()}
            />
          </div>
          <div style={{ display: "flex", gap: "var(--md-space-2)", "flex-wrap": "wrap" }}>
            <Button variant="text" data-testid="organization-settings-entry" onClick={() => navigate("/settings/organization")}>
              Organization settings
            </Button>
            <Button variant="text" data-testid="dashboard-settings-entry" onClick={() => navigate("/settings/dashboard")}>
              Dashboard settings
            </Button>
          </div>
          <Show when={busy()}>
            <span aria-live="polite" style={{ "font-size": "var(--md-body-s-size)", color: "var(--text-lo)" }}>
              Switching dashboard…
            </span>
          </Show>
          <Show when={error()}>
            <span role="alert" style={{ "font-size": "var(--md-body-s-size)", color: "var(--md-sys-color-error)" }}>
              {error()}
            </span>
          </Show>
        </Surface>
      </section>
    </Show>
  );
}

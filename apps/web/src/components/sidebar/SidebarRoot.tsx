// Route-stable primary sidebar owner.
// AppShell keeps this one navigation tree adjacent to every editor route, so
// selection changes with the URL without replacing workspace/session context.
// AllView owns the dense sessions hierarchy and its search surface.


import { AllView } from "./AllView.tsx";

export function SidebarRoot() {
  return <AllView />;
}

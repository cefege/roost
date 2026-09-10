// Compact settings detail surface. SettingsRoot gives it the URL-selected pane specification.
// The back action preserves the push/pop direction and replaces history so list navigation remains stable.
// Pane content stays in SettingsPane, shared with the desktop settings editor.

import { useNavigate } from "@solidjs/router";
import { IconButton } from "./md/primitives.tsx";
import { SettingsPane } from "./SettingsPane.tsx";
import type { SettingsPaneSpec } from "./settingsNavigation.ts";
import { withViewTransition } from "../../lib/viewTransition.ts";

export function MobileSettingsDetail(props: { spec: SettingsPaneSpec }) {
  const navigate = useNavigate();

  function returnToSettingsList() {
    document.documentElement.style.setProperty("--settings-nav-dir", "-1");
    withViewTransition(() => navigate("/settings", { replace: true }));
  }

  return (
    <div class="settings-mobile__main">
      <header class="settings-topbar">
        <IconButton
          class="settings-topbar__back"
          icon="arrow_back"
          label="Back to settings"
          data-testid="settings-detail-back"
          onClick={returnToSettingsList}
        />
        <h1 class="settings-topbar__title">{props.spec.title}</h1>
      </header>
      <div class="settings-content">
        <div class="settings-content__inner">
          <SettingsPane id={props.spec.id} />
        </div>
      </div>
    </div>
  );
}

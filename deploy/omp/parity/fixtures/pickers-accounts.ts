//! Account picker proof scenarios: the real logout and rate-limit-reset selectors, proving the picker
//! properties their describe hooks emit for account rows and the spend confirmation. Registered by pickers.ts.
import type * as LogoutSelector from "@omp-source/packages/tui/src/overlays/logout-account-selector";
import type * as ResetUsageSelector from "@omp-source/packages/tui/src/overlays/reset-usage-selector";
import type { FixtureCase } from "../scenario";
import { pickerClaims } from "./pickers-claims-accounts";
import { below, coversFor, literal } from "./pickers-support";

const overlays = "packages/tui/src/overlays/";
const accounts = pickerClaims["reset-accounts"]!;
const confirm = pickerClaims["reset-confirm"]!;
const signOut = pickerClaims["logout-accounts"]!;
const resetHeading = { terminal: "Spend a saved rate-limit reset", browser: 'h2:text-is("Spend a saved rate-limit reset")' };
const resetRow = { terminal: "idle@example.com", browser: '[data-omp-picker-item="parity:2:c2"]' };
const logoutHeading = { terminal: "Select Parity account to log out", browser: 'h2:text-is("Select Parity account to log out")' };
const logoutRow = { terminal: "personal key", browser: '[data-omp-picker-item="2"]' };

export const fixtureCases: FixtureCase[] = [
  {
    id: "picker-account-reset",
    covers: coversFor(accounts, confirm),
    async create(context) {
      const { ResetUsageSelectorComponent } = await context.load<typeof ResetUsageSelector>(`${overlays}reset-usage-selector.ts`);
      const rows: ResetUsageSelector.ResetUsageAccount[] = [
        { label: "work@example.com", provider: "parity", providerLabel: "Parity", availableCount: 2, redeemableCount: 2, target: { credentialId: 1, provider: "parity", creditId: "c1" }, active: true },
        { label: "idle@example.com", provider: "parity", providerLabel: "Parity", availableCount: 1, redeemableCount: 0, unavailableReason: "cooldown active", target: { credentialId: 2, provider: "parity", creditId: "c2" }, active: false },
      ];
      return new ResetUsageSelectorComponent(rows, () => context.done(), () => context.done());
    },
    checkpoints: [
      {
        name: "accounts",
        comparisons: [
          literal("title", "(Spend a saved rate-limit reset)", { selector: resetHeading.browser }, "Spend a saved rate-limit reset", []),
          literal("account label", "(work@example.com)", { selector: '[data-omp-picker-item="parity:1:c1"] :text-is("work@example.com")' }, "work@example.com", []),
          below("account properties", resetRow, resetHeading, accounts),
        ],
      },
      {
        name: "spend confirmation",
        terminalInput: "\r",
        browserActions: [{ type: "click", selector: 'button:has-text("Spend reset")' }],
        comparisons: [below("confirmation properties", { terminal: "again to spend 1 saved reset", browser: ':text-is("Spend 1 saved reset on work@example.com?")' }, resetHeading, confirm)],
      },
    ],
  },
  {
    id: "picker-account-logout",
    covers: coversFor(signOut),
    async create(context) {
      const { LogoutAccountSelectorComponent } = await context.load<typeof LogoutSelector>(`${overlays}logout-account-selector.ts`);
      const rows: LogoutSelector.LogoutAccount[] = [
        { credentialId: 1, provider: "parity", label: "work@example.com", detail: "Pro plan", type: "oauth", active: true },
        { credentialId: 2, provider: "parity", label: "personal key", detail: "", type: "api_key", active: false },
      ];
      return new LogoutAccountSelectorComponent("Parity", rows, () => context.done(), () => context.done());
    },
    checkpoints: [{
      name: "accounts",
      comparisons: [
        literal("title", "(Select Parity account to log out)", { selector: logoutHeading.browser }, "Select Parity account to log out", []),
        literal("account label", "(work@example.com)", { selector: '[data-omp-picker-item="1"] :text-is("work@example.com")' }, "work@example.com", []),
        below("account properties", logoutRow, logoutHeading, signOut),
      ],
    }],
  },
];

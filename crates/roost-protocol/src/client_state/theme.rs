//! Shared storage keys for account-scoped theme preferences.
//! Consumed by the account-state allowlist and browser appearance engine.
//! Values synchronize through the existing client-state LWW merge.

/// Selected base palette.
pub const BASE_STORAGE_KEY: &str = "roost.theme.base";
/// Selected accent palette.
pub const ACCENT_STORAGE_KEY: &str = "roost.theme.accent";
/// Selected light, dark, or system mode.
pub const MODE_STORAGE_KEY: &str = "roost.theme.mode";
/// All account-scoped theme keys.
pub const KEYS: &[&str] = &[BASE_STORAGE_KEY, ACCENT_STORAGE_KEY, MODE_STORAGE_KEY];

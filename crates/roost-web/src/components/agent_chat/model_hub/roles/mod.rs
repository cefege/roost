//! The models hub's Roles view: OMP's role list, one row per role and one per
//! fallback in its chain, with the keys and mouse of `model-hub.ts`. `rows` builds
//! the list, `edits` returns the settings each key writes, `actions` runs them, and `view`
//! renders the list. Mounted by `model_hub` in the all-folders scope.

mod actions;
mod edits;
mod rows;
mod view;

#[cfg(test)]
mod tests;

pub(super) use edits::{clear_role_in, split_entry};
pub(super) use rows::{chain_in, chat_role_ids, role_title};
pub(super) use view::HubRolesView;

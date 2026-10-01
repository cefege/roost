//! Non-pointer input: TV remotes and game controllers.
//!
//! One modality predicate (`modality`), D-pad focus travel (`spatial` +
//! `spatial_dom`), the Gamepad poll (`pad_mapper` + `gamepad_source`), and the
//! controller router that turns intents into focus moves and
//! [`PadShellAction`]s for the SHELL (`pad_router` over `pad_surfaces`). Every
//! rule is target-independent and native-tested; the `*_dom` modules and the
//! gamepad loop are thin wasm32 adapters.
//!
//! Mounted from the App: [`install_spatial_navigation`] and
//! [`install_gamepad_source`] each return a guard the App drops on unmount.
//! Ports `apps/web/src/browser/gamepadSource.ts` and
//! `apps/web/src/lib/{padActions,padBindings,padFolders,padMode,spatialNavigation,directionalInput,tvMode}.ts`.

pub mod gamepad_source;
pub mod keypad_focus;
pub mod modality;
pub mod pad_bindings;
pub mod pad_folders;
pub mod pad_hints;
pub mod pad_mapper;
pub mod pad_router;
pub mod pad_shell;
pub mod pad_surfaces;
pub mod spatial;

#[cfg(target_arch = "wasm32")]
mod dom_read;
#[cfg(target_arch = "wasm32")]
pub mod modality_dom;
#[cfg(target_arch = "wasm32")]
pub mod pad_dom;
#[cfg(target_arch = "wasm32")]
pub mod spatial_dom;

#[cfg(target_arch = "wasm32")]
pub use gamepad_source::{GamepadSourceGuard, install_gamepad_source};
pub use modality::{ModeChoice, NavModality, device_tv_mode_active};
#[cfg(target_arch = "wasm32")]
pub use modality_dom::{
    apply_nav_modality, load_nav_modality, set_pad_mode_choice, set_tv_mode_choice,
};
pub use pad_bindings::PadAction;
#[cfg(target_arch = "wasm32")]
pub use pad_dom::dispatch_pad_actions;
pub use pad_hints::{PadHintContext, pad_hints};
pub use pad_mapper::PadHeld;
pub use pad_router::{PadActionRouter, PadHints};
pub use pad_surfaces::{PadShellAction, PadSurfaceState, PadSurfaces};
#[cfg(target_arch = "wasm32")]
pub use spatial_dom::{SpatialNavigationGuard, install_spatial_navigation};

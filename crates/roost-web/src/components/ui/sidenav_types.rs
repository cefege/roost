//! Shared state and vocabulary for the sidenav family: the open-state context and
//! the enums whose `as_str` spellings become the `data-*` values that the group
//! selectors in the recipes match on. Used by `sidenav.rs`, `sidenav_layout.rs`,
//! and `sidenav_links.rs`.

use dioxus::prelude::*;

/// Open flag shared by `SidenavWrapper`, `Sidenav`, `SidenavTrigger`, and the rail.
#[derive(Clone, Copy, Debug)]
pub struct SidenavContext {
    pub open: Signal<bool>,
}

impl SidenavContext {
    pub fn toggle(mut self) {
        let next = !*self.open.peek();
        self.open.set(next);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidenavVariant {
    #[default]
    Sidenav,
    Floating,
    Inset,
}

impl SidenavVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sidenav => "Sidenav",
            Self::Floating => "Floating",
            Self::Inset => "Inset",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidenavSide {
    #[default]
    Left,
    Right,
}

impl SidenavSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidenavCollapsible {
    #[default]
    Offcanvas,
    None,
    Icon,
}

impl SidenavCollapsible {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offcanvas => "Offcanvas",
            Self::None => "None",
            Self::Icon => "Icon",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidenavState {
    #[default]
    Expanded,
    Collapsed,
}

impl SidenavState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expanded => "Expanded",
            Self::Collapsed => "Collapsed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidenavMenuButtonVariant {
    #[default]
    Default,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidenavMenuButtonSize {
    #[default]
    Default,
    Sm,
    Lg,
}

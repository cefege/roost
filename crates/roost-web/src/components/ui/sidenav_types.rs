//! Vocabulary for the sidenav menu buttons: the enums whose variants select the
//! recipe `SidenavMenuButton` renders. Used by `sidenav_links.rs`.

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

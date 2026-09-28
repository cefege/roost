//! `IconButton`: an icon-only `Button` with its accessible name and the menu
//! trigger ARIA v2 mapped. Ported from
//! `apps/web/src/components/Settings/md/IconButton.tsx`; used by toolbars, tab
//! strips, card headers and every menu trigger.
//!
//! The label is REQUIRED because an icon-only control with no name is a button a
//! screen reader announces as "button". It becomes `aria-label`; the glyph stays
//! `aria-hidden`.

use dioxus::prelude::*;

use super::button::{Button, ButtonProps, ButtonSize, ButtonVariant};
use super::class_list::class_list;
use super::icon::Icon;

/// The icon-only sizes; a text size on an icon button is not expressible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconButtonSize {
    /// `icon-xs`.
    IconXs,
    /// `icon-sm`.
    IconSm,
    /// `icon`.
    #[default]
    Icon,
    /// `icon-lg`.
    IconLg,
}

impl From<IconButtonSize> for ButtonSize {
    fn from(size: IconButtonSize) -> Self {
        match size {
            IconButtonSize::IconXs => Self::IconXs,
            IconButtonSize::IconSm => Self::IconSm,
            IconButtonSize::Icon => Self::Icon,
            IconButtonSize::IconLg => Self::IconLg,
        }
    }
}

/// The ARIA an icon button carries: its name, and — when it opens a menu — the
/// popup kind, the controlled element and whether it is expanded. An unset menu
/// field is omitted, not written empty, so a plain icon button does not claim a
/// popup.
pub fn icon_button_aria(
    label: &str,
    menu_popup: Option<&str>,
    controls_id: Option<&str>,
    expanded: Option<bool>,
) -> Vec<(&'static str, String)> {
    let mut aria = vec![("aria-label", label.to_string())];
    if let Some(popup) = menu_popup {
        aria.push(("aria-haspopup", popup.to_string()));
    }
    if let Some(controls) = controls_id {
        aria.push(("aria-controls", controls.to_string()));
    }
    if let Some(expanded) = expanded {
        aria.push(("aria-expanded", expanded.to_string()));
    }
    aria
}

/// v2's `IconButtonProps`: the button props minus children and text sizes,
/// plus the icon, the name and the menu-trigger mapping.
#[derive(Props, Clone, PartialEq, Debug)]
pub struct IconButtonProps {
    /// The glyph.
    pub icon: String,
    /// The accessible name.
    pub label: String,
    /// The square size.
    #[props(default)]
    pub size: IconButtonSize,
    /// Defaults to `Ghost`, the toolbar treatment.
    #[props(default = ButtonVariant::Ghost)]
    pub variant: ButtonVariant,
    /// `aria-haspopup` for a menu trigger (`"menu"`, `"listbox"`, `"dialog"`).
    pub menu_popup: Option<String>,
    /// `aria-controls`: the id of the popup this trigger opens.
    pub controls_id: Option<String>,
    /// `aria-expanded`: whether that popup is open.
    pub expanded: Option<bool>,
    /// An extra class appended after `roost-icon-button`.
    pub class: Option<String>,
    /// Activation.
    pub onclick: Option<EventHandler<MouseEvent>>,
    /// Keyboard handling a menu trigger needs.
    pub onkeydown: Option<EventHandler<KeyboardEvent>>,
    /// Pressed before focus moves.
    pub onmousedown: Option<EventHandler<MouseEvent>>,
    /// Pointer-down.
    pub onpointerdown: Option<EventHandler<PointerEvent>>,
    /// Focus arrival.
    pub onfocus: Option<EventHandler<FocusEvent>>,
    /// The mounted element, where v2 passed a `ref`.
    pub onmounted: Option<EventHandler<MountedEvent>>,
    /// Every native button attribute the caller forwards.
    #[props(extends = GlobalAttributes, extends = button)]
    pub attributes: Vec<Attribute>,
}

/// The icon-only button.
#[component]
pub fn IconButton(props: IconButtonProps) -> Element {
    let mut attributes = props.attributes;
    for (name, value) in icon_button_aria(
        &props.label,
        props.menu_popup.as_deref(),
        props.controls_id.as_deref(),
        props.expanded,
    ) {
        attributes.push(Attribute::new(name, value, None, false));
    }
    let class = class_list(["roost-icon-button", props.class.as_deref().unwrap_or("")]);
    let button = ButtonProps {
        variant: props.variant,
        size: ButtonSize::from(props.size),
        icon: None,
        class: Some(class),
        onclick: props.onclick,
        onkeydown: props.onkeydown,
        onmousedown: props.onmousedown,
        onpointerdown: props.onpointerdown,
        onfocus: props.onfocus,
        onmounted: props.onmounted,
        attributes,
        children: rsx! { Icon { name: props.icon } },
    };
    rsx! {
        Button { ..button }
    }
}

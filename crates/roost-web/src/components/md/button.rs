//! `Button`: the native button primitive every action composes. Ported from
//! `apps/web/src/components/Settings/md/Button.tsx`; `IconButton` and every
//! surface's actions build on it. `controls.css` owns the visual treatment.
//!
//! Like v2 it forwards the native button attributes (`disabled`, `title`,
//! `aria-*`, `data-*`, a `type` override) through a spread, and the handful of
//! native events v2's callers attach, so a caller never drops to a raw `<button>`
//! to reach one of them.

use dioxus::prelude::*;

use super::class_list::class_list;
use super::icon::{Icon, IconSize};

/// The six emphasis levels `controls.css` draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// The filled primary action.
    #[default]
    Default,
    /// The tonal secondary action.
    Secondary,
    /// Outlined, for a companion to a primary action.
    Outline,
    /// Text-only, for toolbar and in-row actions.
    Ghost,
    /// A destructive action.
    Destructive,
    /// Styled as an inline link.
    Link,
}

impl ButtonVariant {
    /// The `roost-button--*` modifier.
    pub const fn modifier(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Secondary => "secondary",
            Self::Outline => "outline",
            Self::Ghost => "ghost",
            Self::Destructive => "destructive",
            Self::Link => "link",
        }
    }
}

/// The control heights and icon-only squares `controls.css` draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonSize {
    /// `xs`.
    Xs,
    /// `sm`.
    Sm,
    /// The standard control height.
    #[default]
    Default,
    /// `lg`.
    Lg,
    /// An icon-only square, extra small.
    IconXs,
    /// An icon-only square, small.
    IconSm,
    /// An icon-only square.
    Icon,
    /// An icon-only square, large.
    IconLg,
}

impl ButtonSize {
    /// The `roost-button--*` modifier.
    pub const fn modifier(self) -> &'static str {
        match self {
            Self::Xs => "xs",
            Self::Sm => "sm",
            Self::Default => "default",
            Self::Lg => "lg",
            Self::IconXs => "icon-xs",
            Self::IconSm => "icon-sm",
            Self::Icon => "icon",
            Self::IconLg => "icon-lg",
        }
    }
}

/// The button's class attribute: base, variant, size, then the caller's class.
pub fn button_class(variant: ButtonVariant, size: ButtonSize, class: Option<&str>) -> String {
    let variant_class = format!("roost-button--{}", variant.modifier());
    let size_class = format!("roost-button--{}", size.modifier());
    class_list([
        "roost-button",
        variant_class.as_str(),
        size_class.as_str(),
        class.unwrap_or(""),
    ])
}

/// Whether the caller's forwarded attributes already name the button's `type`.
///
/// v2 defaulted `type` to `"button"` so a button inside a form never submits it
/// by accident, and let a caller override it with `type="submit"`; emitting the
/// default beside an override would put two `type` attributes on one element.
pub fn names_button_type(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| attribute.name == "type")
}

/// The props v2's `ButtonProps` carried: its own four plus every native button
/// attribute and the native events its callers attach.
#[derive(Props, Clone, PartialEq, Debug)]
pub struct ButtonProps {
    /// The emphasis level.
    #[props(default)]
    pub variant: ButtonVariant,
    /// The control size.
    #[props(default)]
    pub size: ButtonSize,
    /// A leading icon ligature, drawn small.
    pub icon: Option<String>,
    /// An extra class appended after the primitive's own.
    pub class: Option<String>,
    /// Activation.
    pub onclick: Option<EventHandler<MouseEvent>>,
    /// Keyboard handling a menu trigger needs (arrow keys open it).
    pub onkeydown: Option<EventHandler<KeyboardEvent>>,
    /// Pressed before focus moves, for controls that must keep focus elsewhere.
    pub onmousedown: Option<EventHandler<MouseEvent>>,
    /// Pointer-down, for press-and-hold and touch-first controls.
    pub onpointerdown: Option<EventHandler<PointerEvent>>,
    /// Focus arrival.
    pub onfocus: Option<EventHandler<FocusEvent>>,
    /// The mounted element, where v2 passed a `ref`.
    pub onmounted: Option<EventHandler<MountedEvent>>,
    /// Every native button attribute the caller forwards.
    #[props(extends = GlobalAttributes, extends = button)]
    pub attributes: Vec<Attribute>,
    /// The label.
    pub children: Element,
}

/// The native button.
#[component]
pub fn Button(props: ButtonProps) -> Element {
    let default_type = (!names_button_type(&props.attributes)).then_some("button");
    let ButtonProps {
        onclick,
        onkeydown,
        onmousedown,
        onpointerdown,
        onfocus,
        onmounted,
        ..
    } = props;
    rsx! {
        button {
            r#type: default_type,
            class: button_class(props.variant, props.size, props.class.as_deref()),
            onclick: move |event| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
            },
            onkeydown: move |event| {
                if let Some(handler) = onkeydown {
                    handler.call(event);
                }
            },
            onmousedown: move |event| {
                if let Some(handler) = onmousedown {
                    handler.call(event);
                }
            },
            onpointerdown: move |event| {
                if let Some(handler) = onpointerdown {
                    handler.call(event);
                }
            },
            onfocus: move |event| {
                if let Some(handler) = onfocus {
                    handler.call(event);
                }
            },
            onmounted: move |event| {
                if let Some(handler) = onmounted {
                    handler.call(event);
                }
            },
            ..props.attributes,
            if let Some(icon) = props.icon {
                Icon { name: icon, size: IconSize::Sm }
            }
            {props.children}
        }
    }
}

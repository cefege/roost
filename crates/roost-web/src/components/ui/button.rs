//! `Button` and `IconButton`: the rust-ui button recipe plus the behaviours Roost's
//! callers rely on: a default `type="button"`, forwarded native attributes and
//! events, and the menu-trigger ARIA. Called by every action, toolbar and menu
//! trigger; depends on `ui::Icon`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::{Icon, IconType};

/// Rust/UI's action emphasis variants.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonVariant {
    #[default]
    Default,
    Destructive,
    Warning,
    Success,
    Outline,
    Secondary,
    Ghost,
    Link,
}

impl ButtonVariant {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90",
            Self::Destructive => {
                "bg-destructive text-white shadow-xs hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:focus-visible:ring-destructive/40 dark:bg-destructive/60"
            }
            Self::Warning => "bg-warning text-warning-foreground hover:bg-warning/90",
            Self::Success => "bg-success text-success-foreground hover:bg-success/90",
            Self::Outline => {
                "border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground dark:bg-input/30 dark:border-input dark:hover:bg-input/5"
            }
            Self::Secondary => {
                "bg-secondary text-secondary-foreground shadow-xs hover:bg-secondary/80"
            }
            Self::Ghost => "hover:bg-accent hover:text-accent-foreground dark:hover:bg-accent/50",
            Self::Link => "text-primary underline-offset-4 hover:underline",
        }
    }
}

/// Rust/UI's text and icon control sizes.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonSize {
    #[default]
    Default,
    Sm,
    Lg,
    Icon,
    IconSm,
    IconXs,
}

impl ButtonSize {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "h-9 px-4 py-2 has-[>svg]:px-3",
            Self::Sm => "h-8 rounded-md gap-1.5 px-3 has-[>svg]:px-2.5",
            Self::Lg => "h-10 rounded-md px-6 has-[>svg]:px-4",
            Self::Icon => "size-9",
            Self::IconSm => "size-8 rounded-md",
            Self::IconXs => "size-6 rounded-md",
        }
    }
}

/// The button's class attribute: the rust-ui base, then variant, size and the
/// caller's class, with later utilities winning conflicts.
pub fn button_class(variant: ButtonVariant, size: ButtonSize, class: Option<&str>) -> String {
    tw_merge!(
        "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-all disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0 [&_svg]:shrink-0 outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-[3px] aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive w-fit hover:cursor-pointer active:scale-[0.98] active:opacity-100 touch-manipulation [-webkit-tap-highlight-color:transparent] select-none [-webkit-touch-callout:none]",
        variant.as_str(),
        size.as_str(),
        class.unwrap_or("")
    )
}

/// Whether the caller's forwarded attributes already name the button's `type`.
///
/// The default `type="button"` keeps a button inside a form from submitting it
/// by accident; a caller's `type="submit"` must replace it, not sit beside it,
/// or the element carries two `type` attributes.
pub fn names_button_type(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| attribute.name == "type")
}

/// Forwards a native event to the caller's handler when it set one.
fn forward<Event: 'static>(handler: Option<EventHandler<Event>>, event: Event) {
    if let Some(handler) = handler {
        handler.call(event);
    }
}

/// The button props: rust-ui's variant, size and class, plus the leading icon
/// and the native events callers attach, and every forwarded native attribute.
#[derive(Props, Clone, PartialEq, Debug)]
pub struct ButtonProps {
    #[props(default)]
    pub variant: ButtonVariant,
    #[props(default)]
    pub size: ButtonSize,
    /// A leading icon, drawn at the recipe's default icon size.
    pub icon: Option<IconType>,
    /// Appended after the recipe, so a caller's utility wins a conflict.
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
    /// The mounted element.
    pub onmounted: Option<EventHandler<MountedEvent>>,
    /// Every native button attribute the caller forwards (`disabled`, `title`,
    /// `aria-*`, `data-*`, `type`).
    #[props(extends = GlobalAttributes, extends = button)]
    pub attributes: Vec<Attribute>,
    pub children: Element,
}

/// The native button.
#[component]
pub fn Button(props: ButtonProps) -> Element {
    let ButtonProps {
        variant,
        size,
        icon,
        class,
        onclick,
        onkeydown,
        onmousedown,
        onpointerdown,
        onfocus,
        onmounted,
        attributes,
        children,
    } = props;
    let default_type = (!names_button_type(&attributes)).then_some("button");
    let class = button_class(variant, size, class.as_deref());
    rsx! {
        button {
            r#type: default_type,
            class: "{class}",
            onclick: move |event| forward(onclick, event),
            onkeydown: move |event| forward(onkeydown, event),
            onmousedown: move |event| forward(onmousedown, event),
            onpointerdown: move |event| forward(onpointerdown, event),
            onfocus: move |event| forward(onfocus, event),
            onmounted: move |event| forward(onmounted, event),
            ..attributes,
            if let Some(icon) = icon {
                Icon { icon }
            }
            {children}
        }
    }
}

/// The ARIA an icon button carries: its name, and, when it opens a menu, the
/// popup kind, the controlled element and whether it is expanded. An unset menu
/// field is omitted, not written empty, so a plain icon button claims no popup.
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

/// The icon-only props: an icon, a required accessible name, and the menu-trigger mapping.
#[derive(Props, Clone, PartialEq, Debug)]
pub struct IconButtonProps {
    /// The glyph.
    pub icon: IconType,
    /// The accessible name; becomes `aria-label`. Required, because an icon-only
    /// control with no name is announced as a bare "button".
    #[props(into)]
    pub label: String,
    /// An icon size; a text size on an icon button is not an icon square.
    #[props(default = ButtonSize::Icon)]
    pub size: ButtonSize,
    /// Defaults to `Ghost`, the toolbar treatment.
    #[props(default = ButtonVariant::Ghost)]
    pub variant: ButtonVariant,
    /// `aria-haspopup` for a menu trigger (`"menu"`, `"listbox"`, `"dialog"`).
    pub menu_popup: Option<String>,
    /// `aria-controls`: the id of the popup this trigger opens.
    pub controls_id: Option<String>,
    /// `aria-expanded`: whether that popup is open.
    pub expanded: Option<bool>,
    /// Appended after the recipe.
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
    /// The mounted element.
    pub onmounted: Option<EventHandler<MountedEvent>>,
    /// Every native button attribute the caller forwards.
    #[props(extends = GlobalAttributes, extends = button)]
    pub attributes: Vec<Attribute>,
}

/// The icon-only button: a `Button` whose glyph is `aria-hidden` and whose name is the label.
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
    let button = ButtonProps {
        variant: props.variant,
        size: props.size,
        icon: None,
        class: props.class,
        onclick: props.onclick,
        onkeydown: props.onkeydown,
        onmousedown: props.onmousedown,
        onpointerdown: props.onpointerdown,
        onfocus: props.onfocus,
        onmounted: props.onmounted,
        attributes,
        children: rsx! {
            Icon { icon: props.icon }
        },
    };
    rsx! {
        Button { ..button }
    }
}

//! Form field layout: `Field` wraps one label, control, description and error,
//! and carries `data-invalid` for the whole group. Also owns the id and
//! `aria-describedby` rules the form controls share, so every control names its
//! description and error the same way.
//!
//! The rules are ported from the md `form_field` primitive: the caller's own ids
//! come first, then the visible description, then the error, so a screen reader
//! reads the caller's context, then the help, then what is wrong.

use crate::components::ui::separator::Separator;
use dioxus::core::current_scope_id;
use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The ids an input's description reads from, space-joined, or `None` when
/// there are none — an empty `aria-describedby` points at nothing.
pub fn described_by(
    caller: Option<&str>,
    description_id: Option<&str>,
    error_id: Option<&str>,
) -> Option<String> {
    let ids: Vec<&str> = [caller, description_id, error_id]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    (!ids.is_empty()).then(|| ids.join(" "))
}

/// The id of a control's description element.
pub fn description_id(control_id: &str) -> String {
    format!("{control_id}-description")
}

/// The id of a control's error element.
pub fn error_id(control_id: &str) -> String {
    format!("{control_id}-error")
}

/// An id unique among the live components, for the component calling it.
///
/// The scope id is unique for as long as the component is mounted, which is
/// exactly how long the `for`/`aria-*` references built from it are read.
pub fn scoped_element_id(prefix: &str) -> String {
    format!("{prefix}-{:?}", current_scope_id()).replace([' ', ',', '"', '(', ')'], "_")
}

#[component]
pub fn FieldSet(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex flex-col gap-6 has-[>[data-name=CheckboxGroup]]:gap-3 has-[>[data-name=RadioGroup]]:gap-3",
        class.as_deref().unwrap_or("")
    );
    rsx! { fieldset { class: "{merged}", {children} } }
}

#[component]
pub fn FieldGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] data_slot: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "group/field-group @container/field-group flex flex-col gap-7 w-full has-[>[data-name=CheckboxGroup]]:gap-3 [&>[data-name=FieldGroup]]:gap-4",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "FieldGroup",
            class: "{merged}",
            "data-slot": data_slot.as_deref(),
            {children}
        }
    }
}

#[component]
pub fn FieldContent(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "group/field-content flex flex-1 flex-col gap-1.5 leading-snug",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { "data-name": "FieldContent", class: "{merged}", {children} }
    }
}

#[component]
pub fn FieldTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex items-center gap-2 text-sm leading-snug font-medium w-fit group-data-[disabled=true]/field:opacity-50",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { class: "{merged}", {children} } }
}

#[component]
pub fn FieldDescription(
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "text-muted-foreground text-sm leading-normal font-normal group-has-[[data-orientation=horizontal]]/field:text-balance last:mt-0 nth-last-2:-mt-1 [[data-variant=legend]+&]:-mt-1.5 [&>a:hover]:text-primary [&>a]:underline [&>a]:underline-offset-4",
        class.as_deref().unwrap_or("")
    );
    rsx! { p { id: id.as_deref(), class: "{merged}", {children} } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FieldLegendVariant {
    #[default]
    Legend,
    Label,
}

#[component]
pub fn FieldLegend(
    #[props(into, optional)] class: Option<String>,
    #[props(default = FieldLegendVariant::default())] variant: FieldLegendVariant,
    children: Element,
) -> Element {
    let variant_attr = match variant {
        FieldLegendVariant::Legend => "legend",
        FieldLegendVariant::Label => "label",
    };
    let merged = tw_merge!(
        "mb-3 font-medium data-[variant=legend]:text-base data-[variant=label]:text-sm",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        legend {
            "data-slot": "field-legend",
            "data-variant": "{variant_attr}",
            class: "{merged}",
            {children}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FieldVariant {
    #[default]
    Vertical,
    Horizontal,
    Responsive,
}

#[component]
pub fn Field(
    #[props(into, optional)] class: Option<String>,
    #[props(default = FieldVariant::default())] variant: FieldVariant,
    #[props(default = false)] invalid: bool,
    #[props(default = false)] disabled: bool,
    children: Element,
) -> Element {
    let variant_class = match variant {
        FieldVariant::Vertical => "flex-col [&>*]:w-full [&>.hidden]:w-auto",
        FieldVariant::Horizontal => {
            "flex-row items-center [&>[data-slot=field-label]]:flex-auto has-[>[data-name=FieldContent]]:items-start has-[>[data-name=FieldContent]]:[&>[role=checkbox],[role=radio]]:mt-px"
        }
        FieldVariant::Responsive => {
            "flex-col [&>*]:w-full [&>.hidden]:w-auto @md/field-group:flex-row @md/field-group:items-center @md/field-group:[&>*]:w-auto @md/field-group:[&>[data-slot=field-label]]:flex-auto @md/field-group:has-[>[data-name=FieldContent]]:items-start @md/field-group:has-[>[data-name=FieldContent]]:[&>[role=checkbox],[role=radio]]:mt-px"
        }
    };
    let merged = tw_merge!(
        "group/field flex gap-3 w-full data-[invalid=true]:text-destructive",
        variant_class,
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "Field",
            class: "{merged}",
            "data-invalid": if invalid { "true" } else { "false" },
            "data-disabled": if disabled { "true" } else { "false" },
            {children}
        }
    }
}

#[component]
pub fn FieldLabel(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] html_for: Option<String>,
    #[props(into, optional)] r#for: Option<String>,
    children: Element,
) -> Element {
    let field_target = html_for.or(r#for).unwrap_or_default();
    let merged = tw_merge!(
        "group/field-label peer/field-label flex gap-2 leading-snug w-fit group-data-[disabled=true]/field:opacity-50 has-[>[data-name=Field]]:w-full has-[>[data-name=Field]]:flex-col has-[>[data-name=Field]]:rounded-md has-[>[data-name=Field]]:border [&>*]:data-[name=Field]:p-4 has-data-[state=checked]:bg-primary/5 has-data-[state=checked]:border-primary dark:has-data-[state=checked]:bg-primary/10 has-[:checked]:bg-primary/5 has-[:checked]:border-primary dark:has-[:checked]:bg-primary/10",
        "text-sm font-medium leading-none select-none peer-disabled:cursor-not-allowed peer-disabled:opacity-50",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        label {
            "data-slot": "field-label",
            r#for: "{field_target}",
            class: "{merged}",
            {children}
        }
    }
}

/// A rule across the field with an optional caption in its middle.
#[component]
pub fn FieldSeparator(
    #[props(into, optional)] class: Option<String>,
    #[props(optional)] children: Option<Element>,
) -> Element {
    let has_content = children.is_some();
    let merged = tw_merge!(
        "relative -my-2 h-5 text-sm group-data-[variant=outline]/field-group:-mb-2",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-slot": "field-separator",
            "data-content": if has_content { "true" } else { "false" },
            class: "{merged}",
            Separator { class: "absolute inset-0 top-1/2" }
            if let Some(c) = children {
                span {
                    class: "block relative px-2 mx-auto bg-background text-muted-foreground w-fit",
                    "data-slot": "field-separator-content",
                    {c}
                }
            }
        }
    }
}

#[component]
pub fn FieldError(
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] class: Option<String>,
    #[props(into, default = "alert".to_owned())] role: String,
    #[props(optional)] children: Option<Element>,
    #[props(optional)] errors: Option<Vec<String>>,
) -> Element {
    let error_class = tw_merge!(
        "text-destructive text-sm font-normal",
        class.as_deref().unwrap_or("")
    );
    if let Some(c) = children {
        return rsx! {
            div { id: id.as_deref(), role: "{role}", "data-slot": "field-error", "data-testid": test_id.as_deref(), class: "{error_class}", {c} }
        };
    }
    if let Some(errs) = errors {
        if errs.is_empty() {
            return rsx! {};
        }
        if errs.len() == 1 {
            let msg = errs.into_iter().next().unwrap_or_default();
            return rsx! {
                div { id: id.as_deref(), role: "{role}", "data-slot": "field-error", class: "{error_class}",
                    "data-testid": test_id.as_deref(),
                    span { "{msg}" }
                }
            };
        }
        return rsx! {
            div { id: id.as_deref(), role: "{role}", "data-slot": "field-error", class: "{error_class}",
                "data-testid": test_id.as_deref(),
                ul { class: "flex flex-col gap-1 ml-4 list-disc",
                    for err in errs {
                        li { "{err}" }
                    }
                }
            }
        };
    }
    rsx! {}
}

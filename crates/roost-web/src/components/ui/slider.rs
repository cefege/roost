//! `Slider`: a native range input. The value is the caller's; every move reports
//! the new number through `on_value_change`, and the element is drawn from the
//! `value` the caller passed, so the browser never holds a value of its own.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderVariant {
    #[default]
    Round,
    Flat,
}

impl SliderVariant {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Round => "round",
            Self::Flat => "flat",
        }
    }
}

#[component]
pub fn Slider(
    #[props(into, optional)] class: Option<String>,
    #[props(default)] variant: SliderVariant,
    #[props(default = 0.0)] min: f64,
    #[props(default = 100.0)] max: f64,
    #[props(default = 1.0)] step: f64,
    #[props(default = 50.0)] value: f64,
    #[props(default = false)] disabled: bool,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(optional)] on_value_change: Option<EventHandler<f64>>,
) -> Element {
    let merged = tw_merge!(
        "overflow-hidden relative bg-transparent transition-all duration-100 ease-in-out appearance-none disabled:opacity-30 disabled:cursor-not-allowed w-48 text-primary active:cursor-grabbing disabled:grayscale",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        input {
            "data-name": "Slider",
            "data-variant": "{variant.as_str()}",
            r#type: "range",
            class: "{merged}",
            min: "{min}",
            max: "{max}",
            step: "{step}",
            value: "{value}",
            disabled,
            "aria-label": aria_label,
            "data-testid": test_id,
            oninput: move |event| {
                if let Some(handler) = &on_value_change && let Ok(parsed) = event.value().parse::<f64>() {
                    handler.call(parsed);
                }
            },
        }
    }
}

//! Settings controls for terminal-only palettes.
//! Selected palettes reach every mounted pane through root CSS variables.
//!
//! User-imported palettes persist on this device and recolor every mounted pane.

use dioxus::prelude::*;
use dioxus::web::WebEventExt as _;
use roost_client_core::KeyValueStore as _;
use wasm_bindgen::JsCast as _;

use crate::platform::LocalStorageKeyValueStore;
use crate::terminal_schemes::{TerminalPalette, TerminalScheme, built_in_schemes, parse_scheme};
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Paper, Text, Textarea, use_combobox,
};
use libero::sx::sx;

const SELECTED_SCHEME_KEY: &str = "roost.terminalColorScheme";
const IMPORTED_SCHEMES_KEY: &str = "roost.importedTerminalColorSchemes";
const MATCH_APP_THEME: &str = "match";

/// The terminal palette controls shown in Settings → Terminal.
#[component]
pub fn TerminalColorSchemes() -> Element {
    let mut selected = use_signal(|| {
        LocalStorageKeyValueStore::new()
            .get(SELECTED_SCHEME_KEY)
            .unwrap_or_else(|| MATCH_APP_THEME.to_owned())
    });
    let mut imported = use_signal(load_imported_schemes);
    let mut draft = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let scheme_select = use_combobox();
    let semibold_text_sx = use_hook(|| libero::sx::sx().font_weight("600"));
    let paper_sx = use_hook(|| sx().padding("md"));
    let scheme_options: Vec<(String, String)> =
        std::iter::once((MATCH_APP_THEME.to_owned(), "Match app theme".to_owned()))
            .chain(
                built_in_schemes()
                    .iter()
                    .map(|scheme| (scheme.id.clone(), scheme.name.clone())),
            )
            .chain(
                imported()
                    .iter()
                    .map(|scheme| (scheme.id.clone(), scheme.name.clone())),
            )
            .collect();

    use_effect(move || {
        let choice = selected();
        let user_schemes = imported();
        LocalStorageKeyValueStore::new().set(SELECTED_SCHEME_KEY, &choice);
        let palette = selected_palette(&choice, &user_schemes);
        apply_palette(palette.as_ref());
    });

    let imported_swatches: Vec<_> = imported()
        .into_iter()
        .map(|scheme| {
            let remove_id = scheme.id.clone();
            let on_delete = EventHandler::new(move |id: String| {
                if id != remove_id {
                    return;
                }
                let mut schemes = imported();
                schemes.retain(|entry| entry.id != id);
                if let Ok(serialized) = serde_json::to_string(&schemes) {
                    LocalStorageKeyValueStore::new().set(IMPORTED_SCHEMES_KEY, &serialized);
                    if selected() == id {
                        selected.set(MATCH_APP_THEME.to_owned());
                    }
                    imported.set(schemes);
                }
            });
            (scheme, on_delete)
        })
        .collect();

    rsx! {
        Paper { sx: paper_sx,
            Flex { direction: "column", gap: "sm",
                Text { size: "lg", sx: semibold_text_sx.clone(), "Color scheme" }
                Flex { direction: "column", gap: "xs",
                    Text { size: "sm", "Color scheme" }
                    Combobox {
                        state: scheme_select,
                        select_only: true,
                        aria_label: "Color scheme",
                        selected_index: scheme_options.iter().position(|(id, _)| *id == selected()),
                        option_label: |option: (String, String)| option.1.clone(),
                        options: scheme_options.clone(),
                        option: {
                            let current = selected();
                            move |row: ComboboxOptionArgs<(String, String)>| {
                                let choice = row.value.0.clone();
                                let selected_id = current.clone();
                                rsx! {
                                    ComboboxOption {
                                        selected: selected_id == row.value.0,
                                        onpick: move |_| {
                                            selected.set(choice.clone());
                                            scheme_select.close();
                                        },
                                        "{row.value.1}"
                                    }
                                }
                            }
                        },
                        Button {
                            attributes: scheme_select.a11y_attributes(),
                            onclick: move |_| scheme_select.toggle(),
                            onblur: move |_| scheme_select.close(),
                            "data-testid": "terminal-color-scheme-select",
                            "{scheme_options.iter().find(|(id, _)| *id == selected()).map(|(_, label)| label.as_str()).unwrap_or_default()}"
                        }
                    }
                }
                Text { size: "sm", color: "muted",
                    "Changes only the terminal palette; app chrome keeps its Light, Dark, or System theme. Applies to every open terminal on this device."
                }
                for scheme in built_in_schemes().iter() {
                    SchemeSwatch { scheme: scheme.clone(), selected: selected() == scheme.id, on_delete: None }
                }
                for (scheme, on_delete) in imported_swatches {
                    SchemeSwatch { scheme: scheme.clone(), selected: selected() == scheme.id, on_delete: Some(on_delete) }
                }
                Textarea {
                    label: "Paste a terminal theme",
                    "data-testid": "terminal-scheme-import",
                    rows: 8,
                    placeholder: "Windows Terminal JSON, iTerm2 .itermcolors XML, kitty or Ghostty key/value",
                    value: Some(draft()),
                    oninput: move |value| { draft.set(value); error.set(None); },
                }
                input {
                    accept: ".json,.itermcolors,.conf,.txt",
                    r#type: "file",
                    "aria-label": "Import terminal color scheme file",
                    "data-testid": "terminal-scheme-file",
                    onchange: move |event: FormEvent| {
                        let Some(web_event) = event.try_as_web_event() else { return; };
                        let Some(file) = web_event.target()
                            .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
                            .and_then(|input| input.files())
                            .and_then(|files| files.get(0)) else { return; };
                        let mut draft = draft;
                        wasm_bindgen_futures::spawn_local(async move {
                            use wasm_bindgen_futures::JsFuture;
                            let Ok(buffer) = JsFuture::from(file.array_buffer()).await else { return; };
                            let Ok(buffer) = buffer.dyn_into::<js_sys::ArrayBuffer>() else { return; };
                            let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                            draft.set(String::from_utf8_lossy(&bytes).into_owned());
                        });
                    },
                }
                if let Some(message) = error() {
                    Text { size: "sm", color: "error", role: "alert", "{message}" }
                }
                Button {
                    variant: "filled",
                    "data-testid": "terminal-scheme-import-button",
                    onclick: move |_| {
                        match parse_scheme(&draft()) {
                            Ok(mut scheme) => {
                                let mut schemes = imported();
                                let mut id_index = schemes.len();
                                loop {
                                    let id = format!("custom-{id_index}");
                                    if schemes.iter().all(|entry| entry.id != id) {
                                        scheme.id = id;
                                        break;
                                    }
                                    id_index = id_index.saturating_add(1);
                                }
                                let choice = scheme.id.clone();
                                schemes.push(scheme);
                                if let Ok(serialized) = serde_json::to_string(&schemes) {
                                    LocalStorageKeyValueStore::new().set(IMPORTED_SCHEMES_KEY, &serialized);
                                    imported.set(schemes);
                                    selected.set(choice);
                                    draft.set(String::new());
                                    error.set(None);
                                }
                            }
                            Err(parse_error) => error.set(Some(parse_error.to_string())),
                        }
                    },
                    "Import scheme"
                }
            }
        }
    }
}

#[component]
fn SchemeSwatch(
    scheme: TerminalScheme,
    selected: bool,
    on_delete: Option<EventHandler<String>>,
) -> Element {
    let palette = scheme.palette;
    let scheme_id = scheme.id;
    let scheme_name = scheme.name;
    let delete_test_id = format!("terminal-scheme-delete-{scheme_id}");
    let preview_label = format!(
        "{scheme_name} palette preview{}",
        if selected { ", selected" } else { "" }
    );
    let preview_sx = use_hook(|| {
        sx().display("flex")
            .width("100%")
            .height("1rem")
            .flex("1")
            .overflow("hidden")
            .border_radius("xs")
    });
    let ansi_segment_sx = use_hook(|| sx().flex("1 1 0%"));
    let accent_segment_sx = use_hook(|| sx().flex("2 1 0%"));
    rsx! {
        Flex { direction: "row", align: "center", gap: "sm",
            Flex {
                direction: "row",
                sx: preview_sx,
                role: "img",
                "aria-label": preview_label,
                style: format!("background: {}; color: {};", palette.background.css(), palette.foreground.css()),
                for color in palette.ansi.iter() {
                    Flex { sx: ansi_segment_sx.clone(), style: format!("background: {};", color.css()), " " }
                }
                Flex { sx: accent_segment_sx.clone(), style: format!("background: {};", palette.cursor.css()), " " }
                Flex { sx: accent_segment_sx, style: format!("background: {};", palette.selection_background.css()), " " }
            }
            Text { size: "sm", "{scheme_name}" }
            if let Some(on_delete) = on_delete {
                Button {
                    variant: "standard",
                    "data-testid": delete_test_id,
                    onclick: move |_| on_delete.call(scheme_id.clone()),
                    "Delete"
                }
            }
        }
    }
}

fn load_imported_schemes() -> Vec<TerminalScheme> {
    LocalStorageKeyValueStore::new()
        .get(IMPORTED_SCHEMES_KEY)
        .and_then(|source| serde_json::from_str(&source).ok())
        .unwrap_or_default()
}

fn selected_palette(choice: &str, imported: &[TerminalScheme]) -> Option<TerminalPalette> {
    if choice == MATCH_APP_THEME {
        return None;
    }
    imported
        .iter()
        .find(|scheme| scheme.id == choice)
        .map(|scheme| scheme.palette.clone())
        .or_else(|| {
            built_in_schemes()
                .iter()
                .find(|scheme| scheme.id == choice)
                .map(|scheme| scheme.palette.clone())
        })
}

fn apply_palette(palette: Option<&TerminalPalette>) {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let style = root.style();
    let mut values = vec![
        (
            "--terminal-grid-bg",
            palette.map(|palette| palette.background),
        ),
        (
            "--terminal-grid-fg",
            palette.map(|palette| palette.foreground),
        ),
        ("--terminal-cursor", palette.map(|palette| palette.cursor)),
        (
            "--terminal-cursor-text",
            palette.map(|palette| palette.cursor_text),
        ),
        (
            "--terminal-selection",
            palette.map(|palette| palette.selection_background),
        ),
        (
            "--terminal-selection-fg",
            palette.map(|palette| palette.selection_foreground),
        ),
    ];
    let ansi_names = [
        "--ansi-black",
        "--ansi-red",
        "--ansi-green",
        "--ansi-yellow",
        "--ansi-blue",
        "--ansi-magenta",
        "--ansi-cyan",
        "--ansi-white",
        "--ansi-bright-black",
        "--ansi-bright-red",
        "--ansi-bright-green",
        "--ansi-bright-yellow",
        "--ansi-bright-blue",
        "--ansi-bright-magenta",
        "--ansi-bright-cyan",
        "--ansi-bright-white",
    ];
    values.extend(
        ansi_names
            .iter()
            .enumerate()
            .map(|(index, name)| (*name, palette.map(|palette| palette.ansi[index]))),
    );
    for (name, color) in values {
        match color {
            Some(color) => {
                let _ = style.set_property(name, &color.css());
            }
            None => {
                let _ = style.remove_property(name);
            }
        }
    }
}

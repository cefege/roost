//! Browser gesture ownership for the canonical OMP prompt composer.
//! File paste and drop are claimed before page-wide terminal drop listeners;
//! all file reading and upload behavior stays in the shared attachment path.
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use dioxus::prelude::*;

pub(super) fn use_files(
    element_id: &str,
    enabled: bool,
    chosen: EventHandler<Vec<ChosenFile>>,
    reading: EventHandler<()>,
    main: Signal<bool>,
) {
    #[cfg(target_arch = "wasm32")]
    {
        use std::{cell::RefCell, rc::Rc};
        let owner = use_hook(|| Rc::new(RefCell::new(None::<browser::FileTarget>)));
        let live_owner = owner.clone();
        let element_id = element_id.to_owned();
        use_effect(use_reactive(
            (&element_id, &enabled),
            move |(element_id, enabled)| {
                live_owner.borrow_mut().take();
                let next = enabled
                    .then(|| browser::FileTarget::attach(&element_id, chosen, reading))
                    .flatten();
                let mut main = main;
                main.set(next.is_some());
                *live_owner.borrow_mut() = next;
            },
        ));
        use_drop(move || {
            owner.borrow_mut().take();
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (element_id, enabled, chosen, reading, main);
}

pub(super) fn focus_composer(element_id: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let Some((field, _)) = browser::composer(element_id) else {
            return false;
        };
        field
            .dyn_ref::<web_sys::HtmlElement>()
            .is_some_and(|field| field.focus().is_ok())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = element_id;
        false
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use crate::components::terminal_chrome::attachment_picker::spawn_read_chosen;
    use wasm_bindgen::{JsCast, closure::Closure};

    pub(super) fn composer(element_id: &str) -> Option<(web_sys::Element, web_sys::Element)> {
        let field = web_sys::window()?
            .document()?
            .get_element_by_id(element_id)?;
        let root = field.closest("[data-omp-native-editor]").ok()??;
        field.closest("[data-omp-role='omp.editor']").ok()??;
        if root.get_attribute("data-omp-editor-mode").as_deref() == Some("PTY")
            || field
                .closest("[inert], [aria-hidden='true']")
                .ok()?
                .is_some()
        {
            return None;
        }
        Some((field, root))
    }

    pub(super) struct FileTarget {
        root: web_sys::Element,
        paste: Closure<dyn FnMut(web_sys::ClipboardEvent)>,
        drag: Closure<dyn FnMut(web_sys::DragEvent)>,
        drop_files: Closure<dyn FnMut(web_sys::DragEvent)>,
    }
    impl FileTarget {
        pub(super) fn attach(
            element_id: &str,
            chosen: EventHandler<Vec<ChosenFile>>,
            reading: EventHandler<()>,
        ) -> Option<Self> {
            let (_, root) = composer(element_id)?;
            let paste = Closure::new(move |event: web_sys::ClipboardEvent| {
                let files = event
                    .clipboard_data()
                    .map(|data| transfer_files(&data))
                    .unwrap_or_default();
                if files.is_empty() {
                    return;
                }
                event.prevent_default();
                event.stop_propagation();
                reading.call(());
                spawn_read_chosen(files, chosen);
            });
            let drag = Closure::new(move |event: web_sys::DragEvent| {
                let Some(transfer) = event.data_transfer() else {
                    return;
                };
                if !transfer
                    .types()
                    .iter()
                    .any(|kind| kind.as_string().as_deref() == Some("Files"))
                {
                    return;
                }
                event.prevent_default();
                event.stop_propagation();
                transfer.set_drop_effect("copy");
            });
            let drop_files = Closure::new(move |event: web_sys::DragEvent| {
                let files = event
                    .data_transfer()
                    .map(|data| transfer_files(&data))
                    .unwrap_or_default();
                if files.is_empty() {
                    return;
                }
                event.prevent_default();
                event.stop_propagation();
                reading.call(());
                spawn_read_chosen(files, chosen);
            });
            let owner = Self {
                root,
                paste,
                drag,
                drop_files,
            };
            owner
                .root
                .add_event_listener_with_callback_and_bool(
                    "paste",
                    owner.paste.as_ref().unchecked_ref(),
                    true,
                )
                .ok()?;
            for name in ["dragenter", "dragover"] {
                owner
                    .root
                    .add_event_listener_with_callback_and_bool(
                        name,
                        owner.drag.as_ref().unchecked_ref(),
                        true,
                    )
                    .ok()?;
            }
            owner
                .root
                .add_event_listener_with_callback_and_bool(
                    "drop",
                    owner.drop_files.as_ref().unchecked_ref(),
                    true,
                )
                .ok()?;
            Some(owner)
        }
    }
    impl Drop for FileTarget {
        fn drop(&mut self) {
            let _ = self.root.remove_event_listener_with_callback_and_bool(
                "paste",
                self.paste.as_ref().unchecked_ref(),
                true,
            );
            for name in ["dragenter", "dragover"] {
                let _ = self.root.remove_event_listener_with_callback_and_bool(
                    name,
                    self.drag.as_ref().unchecked_ref(),
                    true,
                );
            }
            let _ = self.root.remove_event_listener_with_callback_and_bool(
                "drop",
                self.drop_files.as_ref().unchecked_ref(),
                true,
            );
        }
    }
    fn transfer_files(transfer: &web_sys::DataTransfer) -> Vec<web_sys::File> {
        transfer
            .files()
            .map(|files| {
                (0..files.length())
                    .filter_map(|index| files.get(index))
                    .collect()
            })
            .unwrap_or_default()
    }
}

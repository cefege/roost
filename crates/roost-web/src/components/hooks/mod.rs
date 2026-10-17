//! rust-ui's Dioxus hooks (registry `hooks/`), installed where its CLI puts
//! them: beside `ui/`, one hook per file. Each file names what it changes from
//! the registry source and why; components across the client read them.

pub mod use_copy_clipboard;
pub mod use_is_mobile;
pub mod use_media_query;

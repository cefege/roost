//! Uploading attachment chips shared by composers.
//! The caller owns staged files and the upload lifecycle; this component only
//! renders progress and exposes retry or removal.

use dioxus::prelude::*;

use super::{
    Attachment, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentGroup,
    AttachmentMedia, AttachmentMediaVariant, AttachmentOrientation, AttachmentSize,
    AttachmentState, AttachmentTitle, ButtonSize, ButtonVariant, Icon, IconButton, IconType,
    Progress,
};

#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentUploadChipData {
    pub name: String,
    pub size_label: String,
    pub state: AttachmentState,
    pub preview_url: Option<String>,
    pub status: String,
    pub progress: Option<f64>,
    pub in_flight: bool,
    pub done: bool,
    pub retryable: bool,
}

#[component]
pub fn AttachmentUploadChips(
    files: Vec<AttachmentUploadChipData>,
    on_remove: EventHandler<usize>,
    on_retry: EventHandler<usize>,
) -> Element {
    if files.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            class: "w-full min-w-0 px-3 pt-3",
            "data-testid": "attachment-preview-strip",
            "data-composer-part": "attachments",
            role: "list",
            "aria-label": "Attachments uploading or ready to send",
            AttachmentGroup {
                for (index, file) in files.into_iter().enumerate() {
                    AttachmentUploadChip { key: "{index}-{file.name}", file, index, on_remove, on_retry }
                }
            }
        }
    }
}

#[component]
fn AttachmentUploadChip(
    file: AttachmentUploadChipData,
    index: usize,
    on_remove: EventHandler<usize>,
    on_retry: EventHandler<usize>,
) -> Element {
    let size_label = file.size_label.clone();
    let has_preview = file.preview_url.is_some();
    let state_label = file.status.clone();
    rsx! {
        Attachment {
            state: file.state,
            size: if has_preview { AttachmentSize::Default } else { AttachmentSize::Sm },
            orientation: if has_preview { AttachmentOrientation::Vertical } else { AttachmentOrientation::Horizontal },
            class: if has_preview { "" } else { "w-48" },
            "tabindex": "0",
            "role": "listitem",
            "data-testid": "staged-attachment",
            "aria-label": "{file.name}, {size_label}, {state_label}",
            title: "{file.name}, {size_label}, {state_label}",
            if let Some(source) = file.preview_url.as_ref() {
                AttachmentMedia { variant: AttachmentMediaVariant::Image, contain: true,
                    img {
                        "data-testid": "attachment-preview",
                        src: source,
                        alt: file.name.clone(),
                        class: "size-full",
                    }
                }
            } else {
                AttachmentMedia { variant: AttachmentMediaVariant::Icon, class: "text-muted-foreground",
                    Icon { icon: IconType::File }
                }
            }
            AttachmentContent {
                AttachmentTitle { "title": file.name.clone(), "{file.name}" }
                AttachmentDescription { "{state_label}" }
                if file.in_flight || file.done {
                    Progress {
                        value: file.progress,
                        label: "Upload progress for {file.name}",
                        class: "w-full",
                        test_id: "attachment-upload-progress",
                    }
                }
            }
            AttachmentActions {
                if file.retryable {
                    IconButton {
                        icon: IconType::RefreshCw,
                        label: "Retry upload of {file.name}",
                        title: "Retry upload",
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::IconXs,
                        onclick: move |_| on_retry.call(index),
                    }
                }
                IconButton {
                    icon: IconType::X,
                    label: "Remove {file.name}",
                    title: "Remove {file.name}",
                    variant: if has_preview { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                    size: ButtonSize::IconXs,
                    onclick: move |_| on_remove.call(index),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::progress::progress_fill_transform;

    #[test]
    fn progress_chip_uses_the_real_byte_fraction() {
        assert_eq!(progress_fill_transform(0.25), "transform: translateX(-75%)");
    }
}

//! Uploading attachment chips shared by composers.
//! The caller owns staged files and the upload lifecycle; this component only
//! renders progress and exposes retry or removal.

use dioxus::prelude::*;

use super::{
    Attachment, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentGroup,
    AttachmentMedia, AttachmentMediaVariant, AttachmentOrientation, AttachmentSize,
    AttachmentState, AttachmentTitle, Badge, BadgeSize, BadgeVariant, ButtonSize, ButtonVariant,
    Icon, IconButton, IconType, Progress, Spinner,
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

/// The visual density a composer chooses for its image upload chips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttachmentUploadPresentation {
    #[default]
    Standard,
    CompactImages,
}

#[component]
pub fn AttachmentUploadChips(
    files: Vec<AttachmentUploadChipData>,
    on_remove: EventHandler<usize>,
    on_retry: EventHandler<usize>,
    #[props(default)] presentation: AttachmentUploadPresentation,
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
                    AttachmentUploadChip {
                        key: "{index}-{file.name}",
                        file,
                        index,
                        on_remove,
                        on_retry,
                        presentation,
                    }
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
    presentation: AttachmentUploadPresentation,
) -> Element {
    let size_label = file.size_label.clone();
    let has_preview = file.preview_url.is_some();
    let state_label = file.status.clone();
    if presentation == AttachmentUploadPresentation::CompactImages && has_preview {
        return rsx! {
            CompactImageUploadChip { file, index, on_remove, on_retry }
        };
    }
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

/// A terminal image thumbnail with status rendered over media instead of prose.
#[component]
fn CompactImageUploadChip(
    file: AttachmentUploadChipData,
    index: usize,
    on_remove: EventHandler<usize>,
    on_retry: EventHandler<usize>,
) -> Element {
    let Some(source) = file.preview_url.clone() else {
        return rsx! {};
    };
    let state_label = file.status.clone();
    let size_label = file.size_label.clone();
    rsx! {
        Attachment {
            state: file.state,
            size: AttachmentSize::Default,
            orientation: AttachmentOrientation::Vertical,
            class: "w-40 max-lg:landscape:w-24 lg:w-48",
            "tabindex": "0",
            "role": "listitem",
            "data-testid": "staged-attachment",
            "data-presentation": "compact-image",
            "aria-label": "{file.name}, {size_label}, {state_label}",
            title: "{file.name}, {size_label}, {state_label}",
            AttachmentMedia {
                variant: AttachmentMediaVariant::Image,
                contain: true,
                class: "w-full",
                img {
                    "data-testid": "attachment-preview",
                    src: source,
                    alt: file.name.clone(),
                    class: "size-full",
                }
                if file.in_flight {
                    div {
                        class: "absolute inset-0 flex items-center justify-center bg-background/60",
                        role: "status",
                        "aria-label": "Upload {state_label}",
                        if let Some(progress) = file.progress {
                            Progress {
                                value: Some(progress),
                                label: "Upload progress for {file.name}",
                                class: "w-20",
                                test_id: "attachment-upload-progress",
                            }
                        } else {
                            Spinner { class: "text-foreground" }
                        }
                    }
                }
                if file.done {
                    span {
                        class: "absolute right-1 bottom-1 z-10",
                        role: "img",
                        "aria-label": "Upload ready to send",
                        title: "Ready to send",
                        Badge { variant: BadgeVariant::Success, size: BadgeSize::Sm,
                            Icon { icon: IconType::Check, class: "size-3" }
                        }
                    }
                }
                if file.state == AttachmentState::Error {
                    span {
                        class: "absolute right-1 bottom-1 z-10",
                        role: "img",
                        "aria-label": "Upload failed: {state_label}",
                        title: "{state_label}",
                        Badge { variant: BadgeVariant::Destructive, size: BadgeSize::Sm,
                            Icon { icon: IconType::TriangleAlert, class: "size-3" }
                        }
                    }
                }
            }
            AttachmentActions {
                class: "group-data-[orientation=Vertical]/attachment:top-1 group-data-[orientation=Vertical]/attachment:right-1",
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
                    variant: ButtonVariant::Secondary,
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

//! The images this tab uploaded into each terminal session, newest first, so
//! the pane can show a preview strip beside the terminal: the program received
//! only a path, and the operator still wants to see what they sent. The host
//! records an entry once an upload commits and owns each preview URL; removed
//! entries are handed back through [`SentImages::take_removed`] so the host
//! can release them.

use std::collections::{BTreeMap, VecDeque};

/// Previews kept per session; the oldest leaves when another arrives.
pub const SENT_IMAGES_PER_SESSION: usize = 6;

/// One committed image upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentImage {
    /// The upload id, unique per entry.
    pub id: String,
    /// The file name the operator chose.
    pub name: String,
    /// Where the worker stored it.
    pub path: String,
    /// The host-owned preview URL.
    pub preview_url: String,
}

#[derive(Debug, Default)]
pub struct SentImages {
    by_session: BTreeMap<String, VecDeque<SentImage>>,
    removed: Vec<SentImage>,
}

impl SentImages {
    /// Record a committed upload; an over-full session drops its oldest.
    pub fn record(&mut self, session_id: &str, image: SentImage) {
        let images = self.by_session.entry(session_id.to_owned()).or_default();
        images.push_front(image);
        while images.len() > SENT_IMAGES_PER_SESSION {
            if let Some(oldest) = images.pop_back() {
                self.removed.push(oldest);
            }
        }
    }

    /// The session's previews, newest first.
    pub fn for_session(&self, session_id: &str) -> Vec<SentImage> {
        self.by_session
            .get(session_id)
            .map(|images| images.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Dismiss one preview.
    pub fn dismiss(&mut self, session_id: &str, id: &str) {
        if let Some(images) = self.by_session.get_mut(session_id) {
            if let Some(at) = images.iter().position(|image| image.id == id)
                && let Some(image) = images.remove(at)
            {
                self.removed.push(image);
            }
            if images.is_empty() {
                self.by_session.remove(session_id);
            }
        }
    }

    /// Entries that left, whose preview URLs the host must release.
    pub fn take_removed(&mut self) -> Vec<SentImage> {
        std::mem::take(&mut self.removed)
    }
}

#[cfg(test)]
mod tests {
    use super::{SENT_IMAGES_PER_SESSION, SentImage, SentImages};

    fn image(id: usize) -> SentImage {
        SentImage {
            id: id.to_string(),
            name: format!("{id}.png"),
            path: format!("/tmp/{id}.png"),
            preview_url: format!("blob:{id}"),
        }
    }

    #[test]
    fn newest_first_bounded_and_removed_entries_are_handed_back() {
        let mut images = SentImages::default();
        for id in 0..=SENT_IMAGES_PER_SESSION {
            images.record("s", image(id));
        }
        let held = images.for_session("s");
        assert_eq!(held.len(), SENT_IMAGES_PER_SESSION);
        assert_eq!(
            held[0].id,
            SENT_IMAGES_PER_SESSION.to_string(),
            "newest first"
        );
        assert_eq!(images.take_removed(), vec![image(0)], "the oldest left");

        images.dismiss("s", "3");
        assert_eq!(images.take_removed(), vec![image(3)]);
        assert_eq!(images.for_session("s").len(), SENT_IMAGES_PER_SESSION - 1);
        assert!(images.for_session("other").is_empty());
    }
}

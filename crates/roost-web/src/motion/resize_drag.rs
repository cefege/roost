//! Whether a pane divider or the sidebar resizer is being dragged, and the one
//! pointer drag that sets it. Ports `apps/web/src/lib/resizeDrag.ts`; read by the
//! terminal pane's resize claims (suppressed mid-drag), the deck's
//! `data-resizing` and the arrange-settle pulse; begun by `SidebarResizer` and
//! the deck's dividers.
//!
//! Owners are tokens: a release belongs to its own drag only, so a stale or
//! repeated release cannot end another gesture, and a page-lifecycle reset
//! invalidates every token still held.

use std::collections::BTreeSet;

use dioxus::prelude::*;

/// More simultaneous owners than this means some were abandoned; the next begin
/// retires them all.
pub const MAX_RESIZE_DRAG_OWNERS: u8 = 32;

/// One drag's claim on the suppression.
#[derive(Debug, PartialEq, Eq)]
pub struct ResizeDragToken {
    owner: u8,
    generation: u64,
}

/// The live owners, bounded and generation-fenced.
#[derive(Debug, Default)]
pub struct ResizeDragOwners {
    live: BTreeSet<u8>,
    generation: u64,
    next_owner: u8,
}

impl ResizeDragOwners {
    /// Admit one more drag.
    pub fn begin(&mut self) -> ResizeDragToken {
        if self.live.len() >= usize::from(MAX_RESIZE_DRAG_OWNERS) {
            self.live.clear();
            self.generation += 1;
            self.next_owner = 0;
        }
        while self.live.contains(&self.next_owner) {
            self.next_owner = (self.next_owner + 1) % MAX_RESIZE_DRAG_OWNERS;
        }
        let owner = self.next_owner;
        self.next_owner = (self.next_owner + 1) % MAX_RESIZE_DRAG_OWNERS;
        self.live.insert(owner);
        ResizeDragToken {
            owner,
            generation: self.generation,
        }
    }

    /// Release one drag's claim. Consuming the token makes a second release of
    /// the same drag unrepresentable; a token from a retired generation is inert.
    pub fn release(&mut self, token: ResizeDragToken) {
        if token.generation == self.generation {
            self.live.remove(&token.owner);
        }
    }

    /// Retire every owner, including tokens still held (page show / teardown).
    pub fn reset(&mut self) {
        self.generation += 1;
        self.live.clear();
        self.next_owner = 0;
    }

    /// Whether any drag is live.
    pub fn is_dragging(&self) -> bool {
        !self.live.is_empty()
    }
}

/// One pointer resize: the latest sampled geometry and its single coalesced
/// frame. Pointer termination settles the latest sample; disposal aborts it.
#[derive(Debug)]
pub struct PointerResizeSession<T> {
    pointer_id: i32,
    latest: T,
    frame_queued: bool,
    active: bool,
}

/// What finishing a pointer resize owes its host, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeSettlement<T> {
    /// A frame was queued and must be cancelled.
    pub cancel_frame: bool,
    /// The geometry to commit, or `None` for an aborted drag.
    pub commit: Option<T>,
}

impl<T: Copy> PointerResizeSession<T> {
    /// A resize following `pointer_id`, starting at `initial`.
    pub fn new(pointer_id: i32, initial: T) -> Self {
        Self {
            pointer_id,
            latest: initial,
            frame_queued: false,
            active: true,
        }
    }

    /// The pointer this resize follows.
    pub fn pointer_id(&self) -> i32 {
        self.pointer_id
    }

    /// Record a move sample. `true` when the host must request the frame that
    /// will flush it; other pointers and a finished drag are ignored.
    pub fn sample(&mut self, pointer_id: i32, geometry: T) -> bool {
        if !self.active || pointer_id != self.pointer_id {
            return false;
        }
        self.latest = geometry;
        !std::mem::replace(&mut self.frame_queued, true)
    }

    /// The queued frame ran: the geometry to apply live, when still active.
    pub fn frame(&mut self) -> Option<T> {
        self.frame_queued = false;
        self.active.then_some(self.latest)
    }

    /// Whether an up/cancel/lost-capture for `pointer_id` ends this drag.
    pub fn ends_on(&self, pointer_id: i32) -> bool {
        self.active && pointer_id == self.pointer_id
    }

    /// Finish once: `commit` settles the latest sample, otherwise it aborts.
    /// `None` for a drag already finished, so teardown is idempotent.
    pub fn finish(&mut self, commit: bool) -> Option<ResizeSettlement<T>> {
        if !std::mem::replace(&mut self.active, false) {
            return None;
        }
        Some(ResizeSettlement {
            cancel_frame: std::mem::replace(&mut self.frame_queued, false),
            commit: commit.then_some(self.latest),
        })
    }
}

/// The page's resize-drag state, provided once by `App`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeDrag {
    owners: CopyValue<ResizeDragOwners>,
    dragging: Signal<bool>,
    arrange_epoch: Signal<u64>,
}

impl ResizeDrag {
    /// Provide the page's state in the calling (root) scope.
    pub fn provide() -> Self {
        use_context_provider(|| Self {
            owners: CopyValue::new(ResizeDragOwners::default()),
            dragging: Signal::new(false),
            arrange_epoch: Signal::new(0),
        })
    }

    /// Begin one drag; hand the token back to [`ResizeDrag::release`].
    pub fn begin(self) -> ResizeDragToken {
        let token = self.owners.write_unchecked().begin();
        self.publish();
        token
    }

    /// End one drag.
    pub fn release(self, token: ResizeDragToken) {
        self.owners.write_unchecked().release(token);
        self.publish();
    }

    /// Retire every drag (page show).
    pub fn reset(self) {
        self.owners.write_unchecked().reset();
        self.publish();
    }

    /// Whether a drag is live. Subscribes the calling component.
    pub fn is_dragging(self) -> bool {
        (self.dragging)()
    }

    /// The arrange-settle pulse count. Subscribes the calling component.
    pub fn arrange_epoch(self) -> u64 {
        (self.arrange_epoch)()
    }

    /// One arrange preset committed: every visible pane settles its claim once.
    pub fn pulse_arrange(self) {
        let mut epoch = self.arrange_epoch;
        epoch.with_mut(|count| *count += 1);
    }

    fn publish(self) {
        let now = self.owners.read_unchecked().is_dragging();
        if *self.dragging.peek() != now {
            let mut dragging = self.dragging;
            dragging.set(now);
            tracing::debug!(target: "motion", dragging = now, "resize drag suppression changed");
        }
    }
}

/// The page's resize-drag state, from context.
pub fn use_resize_drag() -> ResizeDrag {
    use_context::<ResizeDrag>()
}

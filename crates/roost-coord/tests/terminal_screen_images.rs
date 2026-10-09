//! Inline image placements on the coordinator's canonical replica: a delta
//! that changes the set is folded in, and a late joiner's seed carries it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use roost_proto::PbImagePlacement;
use terminal_screen_hub_support::{STREAM, TestSink, baseline, delta, harness, session, watch};

fn placement(image_key: u64) -> PbImagePlacement {
    PbImagePlacement {
        image_key,
        row: 1,
        col: 2,
        columns: 3,
        rows: 1,
        source_width: 16,
        source_height: 16,
        image_width: 16,
        image_height: 16,
        ..Default::default()
    }
}

#[test]
fn a_late_joiner_is_seeded_with_the_placements_a_delta_set() {
    let h = harness();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &["a", "b"]));

    let mut with_image = delta(1, "b2");
    with_image.image_placements = vec![placement(42)];
    with_image.image_placements_present = true;
    h.frame(with_image);

    let mut text_only = delta(2, "b3");
    text_only.image_placements_present = false;
    h.frame(text_only);

    let late = TestSink::queuing();
    watch(&h.hub, &late, "late");
    assert!(h.hub.seed_socket("late", &session()));
    let seeded = late.last_seeded();
    assert!(seeded.full);
    assert!(seeded.image_placements_present);
    assert_eq!(seeded.image_placements, vec![placement(42)]);
}

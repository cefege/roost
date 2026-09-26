#![allow(clippy::unwrap_used, clippy::expect_used)]
#[test]
fn diagnose_baseline() {
    let proto = roost_proto::PbCellGridFrame {
        stream_id: "stream-1".to_owned(),
        grid_epoch: "grid-1".to_owned(),
        cols: 80,
        rows: 24,
        full: true,
        seq: 1,
        cursor_visible: true,
        viewport_rows: (0..24u32).map(|index| roost_proto::PbCellRow {
            index,
            spans: vec![roost_proto::PbCellSpan {
                text: "$ ".to_owned(), fg: 0, bg: 0, flags: 0, fg_rgb: None, bg_rgb: None,
                columns: 2, link_uri: None, link_key: None, __buffa_unknown_fields: Default::default(),
            }],
            __buffa_unknown_fields: Default::default(),
        }).collect(),
        scrollback_total: 0, sb_base: 0, base_seq: 0,
        ..Default::default()
    };
    println!("assert_snapshot = {:?}", roost_protocol::cell::frame_chunk_validation::assert_cell_grid_snapshot(&proto).map(|s| s.spans));
    println!("proto_to_cell_frame = {:?}", roost_protocol::cell::proto_to_cell_frame(&proto).map(|f| f.rows));
}

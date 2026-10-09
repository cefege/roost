//! The guard on R7: the scalar base64 path Roost builds decodes what the
//! simdutf path does, including a payload with non-zero trailing bits.

#[test]
fn trailing_bits_and_missing_padding_are_forgiven() {
    let canonical = rio_vt::simd_base64::decode(b"/wAA/w==").expect("canonical");
    assert_eq!(canonical, vec![0xff, 0, 0, 0xff]);
    let trailing = rio_vt::simd_base64::decode(b"/wAA//==").expect("trailing bits");
    assert_eq!(trailing, vec![0xff, 0, 0, 0xff]);
    let unpadded = rio_vt::simd_base64::decode(b"/wAA/w").expect("no padding");
    assert_eq!(unpadded, vec![0xff, 0, 0, 0xff]);
}
